//! Box construction: layout children, anonymous blocks, inline formatting contexts.

use style::properties::ComputedValues;
use style::values::computed::{Content, ContentItem};

/// The text a pseudo-element's `content` generates (strings only).
pub(crate) fn content_text(style: &ComputedValues) -> Option<String> {
    match &style.get_counters().content {
        Content::Items(item_data) => {
            let items = &item_data.items[0..item_data.alt_start];
            let mut text = String::new();
            let mut has_string = false;
            for item in items {
                if let ContentItem::String(s) = item {
                    text.push_str(s);
                    has_string = true;
                }
            }
            has_string.then_some(text)
        }
        _ => None,
    }
}
