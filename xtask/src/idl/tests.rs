//! The model the generator works from: grammar normalization and the extended attributes
//! pageon reads.

use super::model;

fn model_of(idl: &str) -> model::Model {
    let dir = std::env::temp_dir().join(format!("pageon-idl-{}-{}", std::process::id(), idl.len()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(dir.join("t.idl"), idl).unwrap_or_else(|e| panic!("{e}"));
    model::load(&dir).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn legacy_factory_functions_are_read() {
    let m = model_of(
        "[Exposed=Window, LegacyFactoryFunction=Image(optional unsigned long width, optional unsigned long height)]\n\
         interface HTMLImageElement { attribute DOMString alt; };",
    );
    let i = &m.interfaces["HTMLImageElement"];
    let (name, args) = i
        .ext
        .factory
        .clone()
        .unwrap_or_else(|| panic!("no factory"));
    assert_eq!(name, "Image");
    assert_eq!(
        args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["width", "height"]
    );
    assert!(args.iter().all(|a| a.optional));
}

#[test]
fn required_members_with_extended_attributes_parse() {
    // The current grammar puts extended attributes after `required` (WebCrypto's dictionaries).
    let m =
        model_of("dictionary RsaParams { required [EnforceRange] unsigned long modulusLength; };");
    assert!(m.dictionaries.contains_key("RsaParams"));
    assert_eq!(
        model::normalize("required [EnforceRange] unsigned long x;"),
        "[EnforceRange] required unsigned long x;"
    );
}
