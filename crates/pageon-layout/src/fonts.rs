//! The bundled font context: Liberation fonts only, never host fonts, so geometry is identical
//! on every machine. Generic families and common web family names map to the metric-compatible
//! Liberation faces.

use std::sync::{Arc, OnceLock};

use parley::FontContext;
use parley::fontique::{Blob, FamilyId, FontInfoOverride, GenericFamily};

macro_rules! font {
    ($file:literal) => {
        (
            $file,
            include_bytes!(concat!("../fonts/", $file)).as_slice(),
        )
    };
}

pub(crate) const FONTS: [(&str, &[u8]); 12] = [
    font!("LiberationSans-Regular.ttf"),
    font!("LiberationSans-Bold.ttf"),
    font!("LiberationSans-Italic.ttf"),
    font!("LiberationSans-BoldItalic.ttf"),
    font!("LiberationSerif-Regular.ttf"),
    font!("LiberationSerif-Bold.ttf"),
    font!("LiberationSerif-Italic.ttf"),
    font!("LiberationSerif-BoldItalic.ttf"),
    font!("LiberationMono-Regular.ttf"),
    font!("LiberationMono-Bold.ttf"),
    font!("LiberationMono-Italic.ttf"),
    font!("LiberationMono-BoldItalic.ttf"),
];

/// Web family names served by each Liberation family.
fn aliases(family: &str) -> &'static [&'static str] {
    match family {
        "LiberationSans" => &["Arial", "Helvetica", "Helvetica Neue", "Liberation Sans"],
        "LiberationSerif" => &["Times New Roman", "Times", "Liberation Serif"],
        _ => &["Courier New", "Courier", "Liberation Mono"],
    }
}

fn blobs() -> &'static Vec<(&'static str, Blob<u8>)> {
    static BLOBS: OnceLock<Vec<(&'static str, Blob<u8>)>> = OnceLock::new();
    BLOBS.get_or_init(|| {
        FONTS
            .iter()
            .map(|(name, bytes)| (*name, Blob::new(Arc::new(bytes.to_vec()))))
            .collect()
    })
}

/// A font context with the bundled fonts, generic families and aliases.
pub(crate) fn context() -> FontContext {
    let mut ctx = FontContext::default();
    let mut families: Vec<(&str, FamilyId)> = Vec::new();
    for (name, blob) in blobs() {
        let family = name.split('-').next().unwrap_or_default();
        for (id, _) in ctx.collection.register_fonts(blob.clone(), None) {
            if !families.iter().any(|(f, _)| *f == family) {
                families.push((family, id));
            }
        }
        for alias in aliases(family) {
            ctx.collection.register_fonts(
                blob.clone(),
                Some(FontInfoOverride {
                    family_name: Some(alias),
                    ..FontInfoOverride::default()
                }),
            );
        }
    }
    let id = |f: &str| families.iter().find(|(n, _)| *n == f).map(|(_, id)| *id);
    let (sans, serif, mono) = (
        id("LiberationSans"),
        id("LiberationSerif"),
        id("LiberationMono"),
    );
    for (generic, family) in [
        (GenericFamily::SansSerif, sans),
        (GenericFamily::SystemUi, sans),
        (GenericFamily::UiSansSerif, sans),
        (GenericFamily::Serif, serif),
        (GenericFamily::UiSerif, serif),
        (GenericFamily::Monospace, mono),
        (GenericFamily::UiMonospace, mono),
    ] {
        if let Some(f) = family {
            ctx.collection
                .set_generic_families(generic, std::iter::once(f));
        }
    }
    ctx
}
