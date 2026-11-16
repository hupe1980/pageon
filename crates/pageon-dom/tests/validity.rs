//! Constraint validation (HTML §4.10.21): the validity rules, barring, radio groups and
//! messages of `pageon_dom::forms`.

use pageon_dom::forms::{self, Validity};
use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};

fn doc(html: &str) -> Dom {
    parse_document(html, ParseOptions::default())
}

fn by_id(dom: &Dom, id: &str) -> NodeId {
    dom.traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no #{id}"))
}

/// Validity and message of `#x` in `html`, after the user typed `typed` (if any).
fn check(html: &str, typed: Option<&str>) -> (Validity, String) {
    let mut dom = doc(html);
    let x = by_id(&dom, "x");
    if let Some(t) = typed {
        let st = dom.control_mut(x);
        st.value = Some(t.into());
        st.user_edited = true;
    }
    let v = forms::validity(&dom, x);
    let m = forms::message(&dom, x, &v);
    (v, m)
}

#[test]
fn value_missing() {
    for (html, msg) in [
        ("<input id=x required>", "Please fill out this field."),
        (
            "<textarea id=x required></textarea>",
            "Please fill out this field.",
        ),
        (
            "<input id=x type=checkbox required>",
            "Please check this box if you want to proceed.",
        ),
        ("<input id=x type=file required>", "Please select a file."),
        (
            "<select id=x required><option value=''>Choose</option><option>A</option></select>",
            "Please select an item in the list.",
        ),
    ] {
        let (v, m) = check(html, None);
        assert!(v.value_missing, "{html}");
        assert_eq!(m, msg, "{html}");
    }
    assert!(check("<input id=x required value=a>", None).0.valid());
    assert!(
        check("<input id=x type=checkbox required checked>", None)
            .0
            .valid()
    );
    assert!(
        check("<select id=x required><option>A</option></select>", None)
            .0
            .valid(),
        "a non-empty first option is not a placeholder"
    );
}

#[test]
fn radio_groups() {
    let mut dom = doc(
        "<form><input type=radio name=r id=a required><input type=radio name=r id=b></form>\
         <input type=radio name=r id=c>",
    );
    let (a, b, c) = (by_id(&dom, "a"), by_id(&dom, "b"), by_id(&dom, "c"));
    assert!(
        forms::validity(&dom, b).value_missing,
        "required on one radio covers the group"
    );
    assert!(
        forms::validity(&dom, c).valid(),
        "another form owner: another group"
    );
    assert_eq!(
        forms::message(&dom, a, &forms::validity(&dom, a)),
        "Please select one of these options."
    );
    dom.control_mut(b).checked = Some(true);
    assert!(forms::validity(&dom, a).valid());
}

#[test]
fn types_patterns_and_lengths() {
    let (v, m) = check("<input id=x type=email>", Some("not-an-email"));
    assert!(v.type_mismatch);
    assert_eq!(m, "Please enter an email address.");
    assert!(
        !m.contains("not-an-email"),
        "messages never quote the value"
    );
    assert!(check("<input id=x type=email>", Some("a@b.test")).0.valid());
    assert!(
        check(
            "<input id=x type=email multiple>",
            Some("a@b.test, c@d.test")
        )
        .0
        .valid()
    );
    assert!(
        check("<input id=x type=url>", Some("example"))
            .0
            .type_mismatch
    );
    assert!(
        check("<input id=x type=url>", Some("https://a.test/"))
            .0
            .valid()
    );

    let (v, m) = check(
        "<input id=x pattern='[0-9]{5}' title='Five digits'>",
        Some("123"),
    );
    assert!(v.pattern_mismatch);
    assert_eq!(m, "Please match the requested format.\nFive digits");
    assert!(
        check("<input id=x pattern='[0-9]{5}'>", Some("12345"))
            .0
            .valid()
    );
    assert!(
        check("<input id=x pattern='a|b'>", Some("ab"))
            .0
            .pattern_mismatch,
        "anchored as a whole"
    );
    assert!(
        check("<input id=x pattern='('>", Some("x")).0.valid(),
        "a broken pattern imposes nothing"
    );

    let (v, m) = check("<input id=x minlength=8>", Some("short"));
    assert!(v.too_short);
    assert_eq!(
        m,
        "Please lengthen this text to 8 characters or more (you are currently using 5 characters)."
    );
    assert!(
        check("<input id=x minlength=8 value=short>", None)
            .0
            .valid(),
        "lengths only after a user edit"
    );
    assert!(check("<input id=x maxlength=3>", Some("abcd")).0.too_long);
}

#[test]
fn ranges_and_steps() {
    let (v, m) = check("<input id=x type=number min=18>", Some("12"));
    assert!(v.range_underflow);
    assert_eq!(m, "Value must be greater than or equal to 18.");
    let (v, m) = check("<input id=x type=number max=10>", Some("11"));
    assert!(v.range_overflow);
    assert_eq!(m, "Value must be less than or equal to 10.");
    let (v, m) = check("<input id=x type=number step=0.5>", Some("1.2"));
    assert!(v.step_mismatch);
    assert_eq!(
        m,
        "Please enter a valid value. The two nearest valid values are 1 and 1.5."
    );
    let (_, m) = check("<input id=x type=number min=0 max=10 step=3>", Some("10"));
    assert_eq!(
        m,
        "Please enter a valid value. The nearest valid value is 9."
    );
    assert!(
        check("<input id=x type=number step=any>", Some("1.234"))
            .0
            .valid()
    );
    let (v, m) = check("<input id=x type=number>", Some("abc"));
    assert!(v.bad_input);
    assert_eq!(m, "Please enter a number.");

    let (v, m) = check("<input id=x type=date min=2024-01-01>", Some("2023-12-31"));
    assert!(v.range_underflow);
    assert_eq!(m, "Value must be 2024-01-01 or later.");
    assert!(
        check("<input id=x type=date>", Some("2024-02-30"))
            .0
            .bad_input
    );
    assert!(
        check("<input id=x type=time step=900>", Some("10:10"))
            .0
            .step_mismatch
    );
    assert!(
        check("<input id=x type=time step=900>", Some("10:15"))
            .0
            .valid()
    );
    assert!(
        check("<input id=x type=month max=2024-06>", Some("2024-07"))
            .0
            .range_overflow
    );
    assert!(
        check("<input id=x type=week>", Some("2024-W53"))
            .0
            .bad_input,
        "2024 has 52 weeks"
    );
    assert!(
        check("<input id=x type=week>", Some("2020-W53")).0.valid(),
        "2020 has 53"
    );
    assert!(
        check("<input id=x type=range max=5>", Some("9")).0.valid(),
        "ranges clamp"
    );
}

#[test]
fn barring() {
    for html in [
        "<input id=x required disabled>",
        "<input id=x required readonly>",
        "<input id=x type=hidden required>",
        "<fieldset disabled><input id=x required></fieldset>",
        "<datalist><input id=x required></datalist>",
        "<button id=x type=reset></button>",
        "<fieldset id=x></fieldset>",
    ] {
        let dom = doc(html);
        let x = by_id(&dom, "x");
        assert!(forms::barred(&dom, x), "{html}");
        assert!(!forms::invalid(&dom, x), "{html}");
    }
    let dom = doc("<fieldset disabled><legend><input id=x required></legend></fieldset>");
    assert!(
        !forms::barred(&dom, by_id(&dom, "x")),
        "the first legend stays enabled"
    );
}

#[test]
fn custom_errors_and_forms() {
    let mut dom =
        doc("<form id=f><fieldset id=s><input id=x></fieldset></form><input id=y form=f>");
    let (f, s, x, y) = (
        by_id(&dom, "f"),
        by_id(&dom, "s"),
        by_id(&dom, "x"),
        by_id(&dom, "y"),
    );
    assert_eq!(
        forms::listed(&dom, f),
        vec![s, x, y],
        "owners by attribute, tree order"
    );
    assert_eq!(forms::owner(&dom, y), Some(f));
    assert!(!forms::has_invalid(&dom, f));
    dom.control_mut(x).custom_error = Some("Passwords do not match".into());
    let v = forms::validity(&dom, x);
    assert!(v.custom_error);
    assert_eq!(forms::message(&dom, x, &v), "Passwords do not match");
    assert!(forms::has_invalid(&dom, f) && forms::has_invalid(&dom, s));
    assert!(!forms::user_invalid(&dom, x));
    dom.control_mut(x).user_validity = true;
    assert!(forms::user_invalid(&dom, x));
}

#[test]
#[allow(clippy::disallowed_methods)] // wall time: SC-003
fn a_thousand_controls_validate_quickly() {
    let mut html = String::from("<form id=f>");
    for i in 0..1000 {
        html.push_str(&format!(
            "<input name=n{i} required pattern='[a-z]+' minlength=2 value=ab><input type=number name=m{i} min=0 max=99 value=5>"
        ));
    }
    let dom = doc(&html);
    let f = by_id(&dom, "f");
    let started = std::time::Instant::now();
    let invalid = forms::listed(&dom, f)
        .into_iter()
        .filter(|&c| forms::invalid(&dom, c))
        .count();
    let took = started.elapsed();
    assert_eq!(invalid, 0);
    let limit = if cfg!(debug_assertions) { 250 } else { 5 };
    assert!(took.as_millis() < limit, "{took:?}");
}
