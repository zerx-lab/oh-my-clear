//! Theme presets bundled into the binary (gpui-component `ThemeSet` JSON, `themes/`).
//!
//! `oh-my-clear.json` is the app's own pair and the default. It is authored in OKLCH and
//! converted to sRGB hex once, when the file was written (the JSON format has no OKLCH):
//! zinc greys at hue 286 with chroma ≤ 0.007 — light: background L 0.985, cards (`group_box`)
//! 1.0, muted 0.955, border 0.915, input border 0.87, muted text 0.52, text 0.21; dark:
//! background 0.16, cards 0.19, popovers 0.215, muted 0.24, border 0.27, input border 0.32,
//! muted text 0.71, text 0.965. The accent is [`super::Accent::Blue`]'s ramp; tones are
//! success h150, warning h60/75, danger h25–27, info h240. Contrast is checked by the
//! theme tests (body ≥ 4.5:1, secondary ≥ 3:1).
//!
//! `one`, `github`, `dracula`, `nord` and `rose-pine` are authored for oh-my-clear from the
//! palettes linked in each file's `url`; the others come from longbridge/gpui-kit v0.7.0
//! `themes/` (Apache-2.0), with keys the schema does not know renamed to the schema's
//! (`window_border` → `window.border`, `link.foreground` → `link`, syntax `comment.doc` →
//! `comment_doc`) and `panel.background` dropped (L-0014).

/// `(file name, JSON)` for every bundled preset file.
pub(crate) const FILES: [(&str, &str); 12] = [
    (
        "oh-my-clear.json",
        include_str!("../../themes/oh-my-clear.json"),
    ),
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
pub const DEFAULT_LIGHT: &str = "oh-my-clear Light";
/// Dark theme used until the user picks another.
pub const DEFAULT_DARK: &str = "oh-my-clear Dark";
