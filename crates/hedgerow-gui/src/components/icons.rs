//! Monochrome inline-SVG icons (Lucide style) that follow the text colour.

use dioxus::prelude::*;

/// Every name [`Icon`] can draw.
#[cfg(test)]
const NAMES: &[&str] = &["panel-close", "panel-open"];

/// The SVG elements drawn for `name`; empty for an unknown name.
fn shapes(name: &str) -> &'static str {
    match name {
        "panel-close" => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/><path d="m16 9-3 3 3 3"/>"#
        }
        "panel-open" => r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>"#,
        _ => "",
    }
}

/// A `size` px icon; `name` is one of the names in [`shapes`], else it
/// draws nothing.
#[component]
pub fn Icon(name: &'static str, #[props(default = 16)] size: u16) -> Element {
    rsx! {
        svg {
            class: "icon",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.75",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            // Static markup from `shapes`, never message content.
            dangerous_inner_html: shapes(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_has_shapes_and_unknown_is_empty() {
        for name in NAMES {
            assert!(!shapes(name).is_empty(), "{name} has no shapes");
        }

        assert!(shapes("nope").is_empty());
    }
}
