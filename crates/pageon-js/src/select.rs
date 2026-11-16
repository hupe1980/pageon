//! CSS selectors over pageon-dom: Stylo's parser and pseudo-classes, `selectors`' matching.

use std::ptr::NonNull;

use pageon_dom::{Dom, Edge, NodeData, NodeId};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::matching::{
    ElementSelectorFlags, MatchingContext, MatchingForInvalidation, MatchingMode,
    NeedsSelectorFlags, QuirksMode, SelectorCaches,
};
use selectors::{OpaqueElement, SelectorList};
use style::selector_parser::{NonTSPseudoClass, PseudoElement, SelectorImpl, SelectorParser};
use style::stylesheets::UrlExtraData;
use style::values::AtomString;
use style::{CaseSensitivityExt, LocalName, Namespace};

use crate::cv::{Error, R};

/// State the pseudo-classes read.
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    /// The focused element.
    pub focused: Option<NodeId>,
    /// Open modal dialogs.
    pub modals: &'a [NodeId],
}

#[derive(Clone, Copy)]
struct El<'a> {
    dom: &'a Dom,
    id: NodeId,
    cx: Ctx<'a>,
}

impl std::fmt::Debug for El<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "El({:?})", self.id)
    }
}

impl<'a> El<'a> {
    fn at(&self, id: NodeId) -> Self {
        Self {
            dom: self.dom,
            id,
            cx: self.cx,
        }
    }

    fn node(&self) -> Option<&'a pageon_dom::Node> {
        self.dom.get(self.id)
    }

    fn is_element(dom: &Dom, id: NodeId) -> bool {
        dom.get(id)
            .is_some_and(|n| matches!(n.data, NodeData::Element { .. }))
    }

    fn attr(&self, name: &str) -> Option<&'a str> {
        self.node()?.attr(name)
    }

    fn is(&self, local: &str) -> bool {
        self.node().is_some_and(|n| n.is_html(local))
    }

    fn checked(&self) -> bool {
        (self.is("input")
            && self.attr("type").is_some_and(|t| {
                t.eq_ignore_ascii_case("checkbox") || t.eq_ignore_ascii_case("radio")
            })
            && self.attr("checked").is_some())
            || (self.is("option") && self.attr("selected").is_some())
    }

    fn disabled(&self) -> bool {
        if pageon_dom::forms::is_form_associated(self.dom, self.id) {
            return pageon_dom::forms::disabled(self.dom, self.id);
        }
        let form = [
            "button", "input", "select", "textarea", "optgroup", "option", "fieldset",
        ]
        .iter()
        .any(|t| self.is(t));
        form && (self.attr("disabled").is_some()
            || std::iter::successors(self.dom.parent(self.id), |&p| self.dom.parent(p)).any(|p| {
                self.dom
                    .get(p)
                    .is_some_and(|n| n.is_html("fieldset") && n.attr("disabled").is_some())
            }))
    }
}

impl selectors::Element for El<'_> {
    type Impl = SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        let key = ((self.id.index() as usize) << 1 | 1).max(1);
        NonNull::new(std::ptr::without_provenance_mut::<()>(key)).map_or_else(
            || OpaqueElement::from_non_null_ptr(NonNull::<()>::dangling()),
            OpaqueElement::from_non_null_ptr,
        )
    }

    fn parent_element(&self) -> Option<Self> {
        self.dom
            .parent(self.id)
            .filter(|&p| Self::is_element(self.dom, p))
            .map(|p| self.at(p))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        std::iter::successors(self.dom.prev_sibling(self.id), |&s| {
            self.dom.prev_sibling(s)
        })
        .find(|&s| Self::is_element(self.dom, s))
        .map(|s| self.at(s))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        std::iter::successors(self.dom.next_sibling(self.id), |&s| {
            self.dom.next_sibling(s)
        })
        .find(|&s| Self::is_element(self.dom, s))
        .map(|s| self.at(s))
    }

    fn first_element_child(&self) -> Option<Self> {
        self.dom
            .children(self.id)
            .find(|&c| Self::is_element(self.dom, c))
            .map(|c| self.at(c))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.node()
            .is_some_and(|n| n.namespace() == Some(pageon_dom::Namespace::Html))
    }

    fn has_local_name(&self, local_name: &html5ever::LocalName) -> bool {
        match self.node().map(|n| &n.data) {
            Some(NodeData::Element { name, .. }) => name.local == *local_name,
            _ => false,
        }
    }

    fn has_namespace(&self, ns: &html5ever::Namespace) -> bool {
        match self.node().map(|n| &n.data) {
            Some(NodeData::Element { name, .. }) => name.ns == *ns,
            _ => false,
        }
    }

    fn is_same_type(&self, other: &Self) -> bool {
        match (self.node().map(|n| &n.data), other.node().map(|n| &n.data)) {
            (Some(NodeData::Element { name: a, .. }), Some(NodeData::Element { name: b, .. })) => {
                a.local == b.local && a.ns == b.ns
            }
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        local_name: &LocalName,
        operation: &AttrSelectorOperation<&AtomString>,
    ) -> bool {
        let Some(n) = self.node() else { return false };
        n.attrs().iter().any(|a| {
            let q = a.qual_name();
            q.local == local_name.0
                && match ns {
                    NamespaceConstraint::Any => true,
                    NamespaceConstraint::Specific(ns) => q.ns == ns.0,
                }
                && operation.eval_str(&a.value)
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &NonTSPseudoClass,
        _: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        match pc {
            NonTSPseudoClass::Checked => self.checked(),
            NonTSPseudoClass::Disabled => self.disabled(),
            NonTSPseudoClass::Enabled => {
                [
                    "button", "input", "select", "textarea", "optgroup", "option", "fieldset",
                ]
                .iter()
                .any(|t| self.is(t))
                    && !self.disabled()
            }
            NonTSPseudoClass::AnyLink | NonTSPseudoClass::Link => {
                (self.is("a") || self.is("area")) && self.attr("href").is_some()
            }
            NonTSPseudoClass::Focus | NonTSPseudoClass::FocusVisible => {
                self.cx.focused == Some(self.id)
            }
            NonTSPseudoClass::FocusWithin => self
                .cx
                .focused
                .is_some_and(|f| self.dom.is_inclusive_ancestor(self.id, f)),
            NonTSPseudoClass::Required => {
                (self.is("input") || self.is("select") || self.is("textarea"))
                    && self.attr("required").is_some()
            }
            NonTSPseudoClass::Optional => {
                (self.is("input") || self.is("select") || self.is("textarea"))
                    && self.attr("required").is_none()
            }
            NonTSPseudoClass::Valid | NonTSPseudoClass::Invalid => {
                use pageon_dom::forms;
                let invalid = if self.is("form") || self.is("fieldset") {
                    Some(forms::has_invalid(self.dom, self.id))
                } else {
                    (!forms::barred(self.dom, self.id))
                        .then(|| !forms::validity(self.dom, self.id).valid())
                };
                invalid.is_some_and(|i| i == matches!(pc, NonTSPseudoClass::Invalid))
            }
            NonTSPseudoClass::UserInvalid => pageon_dom::forms::user_invalid(self.dom, self.id),
            NonTSPseudoClass::UserValid => {
                use pageon_dom::forms;
                self.dom.control(self.id).is_some_and(|c| c.user_validity)
                    && !forms::barred(self.dom, self.id)
                    && forms::validity(self.dom, self.id).valid()
            }
            NonTSPseudoClass::CustomState(state) => {
                let name = state.0.to_string();
                self.dom
                    .internals(self.id)
                    .is_some_and(|i| i.states.contains(&name))
            }
            NonTSPseudoClass::ReadOnly => {
                !(self.is("input") || self.is("textarea")) || self.attr("readonly").is_some()
            }
            NonTSPseudoClass::ReadWrite => {
                (self.is("input") || self.is("textarea")) && self.attr("readonly").is_none()
            }
            NonTSPseudoClass::PlaceholderShown => {
                self.attr("placeholder").is_some() && self.attr("value").is_none_or(str::is_empty)
            }
            NonTSPseudoClass::Modal => self.cx.modals.contains(&self.id),
            NonTSPseudoClass::PopoverOpen => self.dom.popover_open(self.id),
            NonTSPseudoClass::Open => {
                (self.is("details") || self.is("dialog")) && self.attr("open").is_some()
            }
            NonTSPseudoClass::Defined => true,
            _ => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _: &PseudoElement,
        _: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, _: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        (self.is("a") || self.is("area")) && self.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &style::values::AtomIdent, cs: CaseSensitivity) -> bool {
        self.attr("id")
            .is_some_and(|v| cs.eq_atom(&style::Atom::from(v), id))
    }

    fn has_class(&self, name: &style::values::AtomIdent, cs: CaseSensitivity) -> bool {
        self.attr("class").is_some_and(|c| {
            c.split_ascii_whitespace()
                .any(|v| cs.eq_atom(&style::Atom::from(v), name))
        })
    }

    fn imported_part(&self, _: &style::values::AtomIdent) -> Option<style::values::AtomIdent> {
        None
    }

    fn is_part(&self, _: &style::values::AtomIdent) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.dom
            .children(self.id)
            .all(|c| match self.dom.get(c).map(|n| &n.data) {
                Some(NodeData::Element { .. }) => false,
                Some(NodeData::Text(t)) => t.is_empty(),
                _ => true,
            })
    }

    fn is_root(&self) -> bool {
        self.dom.parent(self.id).is_some_and(|p| {
            self.dom
                .get(p)
                .is_some_and(|n| matches!(n.data, NodeData::Document))
        })
    }

    fn has_custom_state(&self, _: &style::values::AtomIdent) -> bool {
        false
    }

    fn add_element_unique_hashes(&self, _: &mut selectors::bloom::BloomFilter) -> bool {
        false
    }
}

/// A parsed selector list.
pub struct Selectors(SelectorList<SelectorImpl>);

/// Parses a selector list; invalid input is a `SyntaxError`.
pub fn parse(selector: &str) -> R<Selectors> {
    let url =
        UrlExtraData::from(url::Url::parse("about:blank").map_err(|e| Error::Type(e.to_string()))?);
    SelectorParser::parse_author_origin_no_namespace(selector, &url)
        .map(Selectors)
        .map_err(|_| {
            Error::dom(
                "SyntaxError",
                format!("'{selector}' is not a valid selector."),
            )
        })
}

impl Selectors {
    /// Whether element `id` matches.
    pub fn matches(&self, dom: &Dom, id: NodeId, cx: Ctx<'_>) -> bool {
        if !El::is_element(dom, id) {
            return false;
        }
        let mut caches = SelectorCaches::default();
        let mut mc = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        selectors::matching::matches_selector_list(&self.0, &El { dom, id, cx }, &mut mc)
    }

    /// Matching descendants of `root` in tree order (`first`: stop at the first).
    pub fn query(&self, dom: &Dom, root: NodeId, cx: Ctx<'_>, first: bool) -> Vec<NodeId> {
        let mut out = Vec::new();
        for e in dom.traverse(root) {
            let Edge::Enter(id) = e else { continue };
            if id != root && self.matches(dom, id, cx) {
                out.push(id);
                if first {
                    break;
                }
            }
        }
        out
    }
}
