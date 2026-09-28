//! Theme presets bundled into the binary (gpui-component `ThemeSet` JSON, `themes/`).
//!
//! `one`, `github`, `dracula`, `nord` and `rose-pine` are authored for oh-my-clear from the
//! palettes linked in each file's `url`; the others come from longbridge/gpui-kit v0.7.0
//! `themes/` (Apache-2.0), with keys the schema does not know renamed to the schema's
//! (`window_border` → `window.border`, `link.foreground` → `link`, syntax `comment.doc` →
//! `comment_doc`) and `panel.background` dropped (L-0014).

/// `(file name, JSON)` for every bundled preset file.
pub(crate) const FILES: [(&str, &str); 11] = [
    ("one.json", include_str!("../../themes/one.json")),
    ("github.json", include_str!("../../themes/github.json")),
    (
        "catppuccin.json",
        include_str!("../../themes/catppuccin.json"),
    ),
    (
        "tokyonight.json",
        include_str!("../../themes/tokyonight.json"),
    ),
    (
        "rose-pine.json",
        include_str!("../../themes/rose-pine.json"),
    ),
    ("dracula.json", include_str!("../../themes/dracula.json")),
    ("nord.json", include_str!("../../themes/nord.json")),
    ("gruvbox.json", include_str!("../../themes/gruvbox.json")),
    (
        "everforest.json",
        include_str!("../../themes/everforest.json"),
    ),
    (
        "solarized.json",
        include_str!("../../themes/solarized.json"),
    ),
    ("ayu.json", include_str!("../../themes/ayu.json")),
];

/// Light theme used until the user picks another.
pub const DEFAULT_LIGHT: &str = "One Light";
/// Dark theme used until the user picks another.
pub const DEFAULT_DARK: &str = "One Dark";
