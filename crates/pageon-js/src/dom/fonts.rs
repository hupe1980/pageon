//! `document.fonts` (CSS Font Loading): pages lay out with the bundled fonts, which are always
//! loaded, so the set is ready at once and every valid font checks.

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

/// A `font` value (`bold 16px Inter`); anything else is a `SyntaxError`, as in Chrome.
fn valid(font: &str) -> R<()> {
    pageon_layout::CanvasFont::parse(font)
        .map(|_| ())
        .ok_or_else(|| {
            Error::dom(
                "SyntaxError",
                format!("Could not resolve '{font}' as a font."),
            )
        })
}

/// `FontFaceSource`: `document.fonts`.
pub mod font_face_source {
    use super::*;

    pub fn get_fonts(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "fonts",
            Iface::FontFaceSet,
            Slot::Fonts,
        ))
    }
}

/// `FontFaceSet`.
pub mod font_face_set {
    use super::*;

    pub fn get_ready(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let set = cx.wrap(this).into();
        crate::dom::net::settled(cx, Ok(set))
    }

    pub fn get_status(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("loaded".into())
    }

    pub fn check(_cx: &mut Cx<'_, '_, '_>, _this: Obj, font: String, _text: String) -> R<bool> {
        valid(&font)?;
        Ok(true)
    }

    pub fn load(cx: &mut Cx<'_, '_, '_>, _this: Obj, font: String, _text: String) -> R<Value> {
        match valid(&font) {
            Ok(()) => {
                let none = v8::Array::new(cx.scope, 0).into();
                crate::dom::net::settled(cx, Ok(none))
            }
            Err(e) => crate::dom::net::settled(cx, Err(e)),
        }
    }
}
