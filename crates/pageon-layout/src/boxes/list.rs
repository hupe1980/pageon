//! Adapted from Blitz (blitz-dom `layout/list.rs`, MIT OR Apache-2.0).
//!
//! List item markers.

use html5ever::local_name;
use parley::FontFamily;
use style::computed_values::list_style_position::T as ListStylePosition;
use style::computed_values::list_style_type::T as ListStyleType;
use style::counter_style::CounterStyle;

use super::construct::{Construct, has_element_data};
use super::parley_style;
use super::tree::{BoxId, ListItemLayout, ListItemLayoutPosition, Marker};

/// The bundled bullet font first.
pub(crate) const BULLET_FONT_FAMILY: &str = "Bullet, monospace, sans-serif";

/// Gives the list items among the children of `id` (and, below a list item, its descendants)
/// their markers, counting from `index`.
pub(crate) fn collect_list_item_children(
    cx: &mut Construct<'_>,
    index: &mut usize,
    reversed: bool,
    id: BoxId,
) {
    let mut children = cx.tree[id].children.clone();
    if reversed {
        children.reverse();
    }
    for child in children {
        if let Some(layout) = node_list_item_child(cx, child, *index) {
            cx.tree[child].list_item = Some(Box::new(layout));
            *index += 1;
            collect_list_item_children(cx, index, reversed, child);
        } else if has_element_data(&cx.tree[child]) {
            cx.tree[child].list_item = None;
        }
    }
}

/// The marker of a `display: list-item` child.
fn node_list_item_child(cx: &mut Construct<'_>, id: BoxId, index: usize) -> Option<ListItemLayout> {
    let n = &cx.tree[id];
    let styles = n.primary_styles()?;
    if !styles.get_box().display.is_list_item() {
        return None;
    }
    // A list inside a list item counts on its own.
    if has_element_data(n) && matches!(n.tag, local_name!("ol") | local_name!("ul")) {
        return None;
    }
    let styles = styles.clone();
    let list_style_type = styles.clone_list_style_type();
    let marker = marker_for_style(list_style_type.clone(), index)?;
    let position = match styles.clone_list_style_position() {
        ListStylePosition::Inside => ListItemLayoutPosition::Inside,
        ListStylePosition::Outside => {
            let mut parley_style = parley_style::style(id, &styles);
            if let Some(family) = font_for_bullet_style(list_style_type) {
                parley_style.font_family = family;
            }
            let mut builder = cx.layouts.tree_builder(cx.fonts, 1.0, true, &parley_style);
            match &marker {
                Marker::Char(char) => {
                    let mut buf = [0u8; 4];
                    builder.push_text(char.encode_utf8(&mut buf));
                }
                Marker::String(s) => builder.push_text(s),
            }
            let mut layout = builder.build().0;
            let width = layout.calculate_content_widths().max;
            layout.break_all_lines(Some(width));
            ListItemLayoutPosition::Outside(Box::new(layout))
        }
    };
    Some(ListItemLayout { marker, position })
}

/// The marker for a list style type.
fn marker_for_style(list_style_type: ListStyleType, index: usize) -> Option<Marker> {
    Some(match list_style_type.0 {
        CounterStyle::None => return None,
        CounterStyle::Name(name) => match &*name.0 {
            "lower-alpha" => {
                let mut marker = String::new();
                build_alpha_marker(index, &mut marker);
                Marker::String(format!("{marker}. "))
            }
            "upper-alpha" => {
                let mut marker = String::new();
                build_alpha_marker(index, &mut marker);
                Marker::String(format!("{}. ", marker.to_ascii_uppercase()))
            }
            "decimal" => Marker::String(format!("{}. ", index + 1)),
            "disc" => Marker::Char('•'),
            "circle" => Marker::Char('◦'),
            "square" => Marker::Char('▪'),
            "disclosure-open" => Marker::Char('▾'),
            "disclosure-closed" => Marker::Char('▸'),
            _ => Marker::Char('□'),
        },
        CounterStyle::String(s) => Marker::String(s.as_ref().to_string()),
        CounterStyle::Symbols { symbols, .. } => {
            let syms = &symbols.0;
            if syms.is_empty() {
                Marker::Char('•')
            } else {
                // Cycles through the symbols.
                let sym = &syms[if syms.len() > 1 {
                    index % syms.len()
                } else {
                    0
                }];
                Marker::String(match sym {
                    style::counter_style::Symbol::String(s) => s.to_string(),
                    style::counter_style::Symbol::Ident(id) => id.0.to_string(),
                })
            }
        }
    })
}

/// Bullets are drawn with the bullet font.
fn font_for_bullet_style(list_style_type: ListStyleType) -> Option<FontFamily<'static>> {
    list_style_type
        .0
        .is_bullet()
        .then(|| BULLET_FONT_FAMILY.into())
}

const ALPHABET: [char; 26] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z',
];

/// The alphabetic marker of `index` (`a`…`z`, `aa`…).
fn build_alpha_marker(index: usize, out: &mut String) {
    let rem = index % 26;
    out.insert(0, ALPHABET[rem]);
    let rest = (index - rem) as i64 / 26 - 1;
    if rest >= 0 {
        build_alpha_marker(rest as usize, out);
    }
}
