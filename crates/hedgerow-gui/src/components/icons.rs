//! Monochrome inline-SVG icons (Lucide style) that follow the text colour.

use dioxus::prelude::*;

/// Every name [`Icon`] can draw.
#[cfg(test)]
const NAMES: &[&str] = &["bell-off", "panel-close", "panel-open"];

/// The SVG elements drawn for `name`; empty for an unknown name.
fn shapes(name: &str) -> &'static str {
    match name {
        "bell-off" => {
            r#"<path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M17 17H4a1 1 0 0 1-.74-1.673C4.59 13.956 6 12.499 6 8a6 6 0 0 1 .258-1.742"/><path d="m2 2 20 20"/><path d="M8.668 3.01A6 6 0 0 1 18 8c0 2.687.77 4.653 1.707 6.05"/>"#
        }
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
