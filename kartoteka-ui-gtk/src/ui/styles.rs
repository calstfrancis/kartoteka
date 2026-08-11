//! Single home for Kartoteka's static, app-wide CSS.

/// The suite's shared interface layer, vendored from fond-style. Loaded before
/// GLOBAL_CSS so app rules can still override it. Do not edit the copy in
/// `style/` — change it in fond-style and run its `sync.sh`, or the next sync
/// silently reverts you.
const FOND_CSS: &str = include_str!("../../style/fond.css");

/// App-specific additions, layered after `FOND_CSS`. Tag chips aren't part of the shared
/// suite vocabulary (no other Fond app has a tagging UI), so this is the one rule Kartoteka
/// owns outright.
const GLOBAL_CSS: &str = "\
    .tag-chip { \
        background: alpha(@window_fg_color, 0.08); \
        border-radius: 999px; \
        padding: 1px 9px; \
        font-size: 0.85em; \
    }";

/// Loads all static, app-wide CSS once, at startup.
pub fn load_global_css() {
    let css = gtk4::CssProvider::new();
    css.load_from_data(&format!("{FOND_CSS}\n{GLOBAL_CSS}"));
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
