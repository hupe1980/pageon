//! `matchMedia`: media queries evaluated by Stylo, as CSS `@media` is, against the device of a
//! viewport (kept per thread: one small document per viewport size).

use std::cell::RefCell;

use blitz_dom::BaseDocument;
use cssparser::Parser;
use selectors::matching::QuirksMode;
use style::media_queries::MediaList;
use style::parser::ParserContext;
use style::servo_arc::Arc as ServoArc;
use style::stylesheets::{CssRuleType, CustomMediaEvaluator, Origin, UrlExtraData};
use style_traits::{ParsingMode, ToCss};

use crate::Viewport;

thread_local! {
    static DEVICE: RefCell<Option<(Viewport, BaseDocument)>> = const { RefCell::new(None) };
}

/// A media query list as `MediaQueryList.media` serializes it, and whether it matches at
/// `viewport` (`not all` and `false` for an invalid query).
pub fn media_query(viewport: Viewport, query: &str) -> (String, bool) {
    let url = UrlExtraData(ServoArc::new(
        url::Url::parse("about:blank").unwrap_or_else(|_| unreachable!("a valid URL")),
    ));
    let mut context = ParserContext::new(
        Origin::Author,
        &url,
        Some(CssRuleType::Media),
        ParsingMode::DEFAULT,
        QuirksMode::NoQuirks,
        Default::default(),
        None,
        None,
        Default::default(),
    );
    let mut parser = Parser::new(query);
    let list = MediaList::parse(&mut context, &mut parser);
    let media = list.to_css_string().to_string();
    let matches = DEVICE.with(|d| {
        let mut d = d.borrow_mut();
        if d.as_ref().is_none_or(|(v, _)| *v != viewport) {
            *d = Some((viewport, crate::mirror::empty(viewport)));
        }
        let Some((_, doc)) = d.as_mut() else {
            return false;
        };
        let device = doc.stylist_device();
        list.evaluate(
            device,
            QuirksMode::NoQuirks,
            &mut CustomMediaEvaluator::none(),
        )
    });
    (media, matches)
}
