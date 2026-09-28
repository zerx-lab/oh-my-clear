---
status: accepted
date: 2026-09-28
tags: [gui, design, i18n, deps]
---
# 0013 UI foundation: gpui-kit theme presets + dial style layer, rust-i18n locales, TitleBar-based chrome

## Context and Problem Statement
The first dial window needs a theme system every component follows (gpui-kit's and dial's), light/dark with One Dark and popular community palettes, user control over shape details (corners, shadows, focus ring, dividers, fonts, scrollbars, accent), Chinese/English UI following the OS, and a custom titlebar that shows native traffic lights on macOS and drawn controls on Windows/Linux. ADR 0011 planned colour tokens generated in OKLCH from `base_hue/accent/contrast`; the user asked for community palettes instead.

## Considered Options
* Own token set generated in OKLCH (ADR 0011 plan), mapped onto gpui-kit's `Theme` by hand
* gpui-component `ThemeConfig` JSON presets as the colour source, plus a dial style layer on `Theme` (**chosen**)
* Own titlebar drawing traffic lights / caption buttons vs gpui-component `TitleBar` (**chosen**)
* Locale: rust-i18n (already required by gpui-component) + `sys-locale` for OS detection (**chosen**) vs hand-reading `LANG`/platform APIs (Windows needs `unsafe`)

## Decision Outcome
- **Colour source**: palettes are gpui-component `ThemeSet` JSON in `crates/dial-ui/themes/`, bundled with `include_str!` and registered in `ThemeRegistry`. Default pair One Light / One Dark. Every component reads the global `Theme` (`cx.theme()`); dial views never hold literal colours.
- **Layers** (`dial_ui::theme::apply`, re-run on any change): preset for each mode → `Appearance` (System/Light/Dark, System follows `observe_window_appearance`) → `ThemeStyle` overriding every `Theme` knob gpui-kit exposes (radius/radius_lg via `CornerStyle`, shadow, focus_ring, font families and sizes, scrollbar mode, sheet margin) plus dial-only `dividers`. Style writes go through `Theme::update` *after* `Theme::change`, because `change` reloads the preset and resets colours.
- **OKLCH stays for dial-authored colour**: the optional `Accent` ramp (primary/ring/link/selection family) is authored in OKLCH and converted once at apply time; ramps are tested for ≥ 4.5:1 text contrast. This amends ADR 0011's "generate all tokens from OKLCH".
- **Registry reloads**: gpui-component's own `ThemeRegistry` observer re-applies the bare preset; dial registers a second observer (after `gpui_kit::init`) that re-applies the style layer.
- **Preferences**: one `UiSettings` global; every write goes through `UiSettings::update`, which applies locale/theme/menus and refreshes windows. Where preferences persist is open (open-questions.md).
- **i18n**: `rust_i18n::i18n!("locales")` in dial-ui (`locales/app.yml`, `en` + `zh-CN`); `Language::System` maps the `sys-locale` tag (any `zh*` → `zh-CN`, else `en`). gpui-component strings follow the same `set_locale`.
- **Chrome**: `AppTitleBar` wraps gpui-component `TitleBar` at `tokens::chrome::TITLE_BAR_HEIGHT` (40 px); windows use `appears_transparent`, traffic lights inset 12 px, `app_owns_titlebar_drag`, `WindowDecorations::Client` (Linux CSD; gpui-component's Root adds shadow + resize edges). Every command is an `Action` with a key binding and a native menu entry.
- **Fonts**: Inter 4.1 and JetBrains Mono 2.304 static TTFs (400/500/600) bundled in `crates/dial-ui/assets/fonts/` with their OFL texts, registered before the first window.

### Consequences
* Good, because community palettes drop in as JSON, every gpui-kit component follows them, and dial's style knobs apply uniformly on top.
* Good, because the one fragile ordering (style after preset reload) is covered by a headless `#[gpui_kit::test]`.
* Bad, because gpui-component theme JSON silently ignores unknown keys; upstream palettes carry misspelled keys (L-0014), so presets need the key check in their authoring.
* Bad, because a border *width* is not themable in gpui-kit 0.7; only dial's chrome dividers can be toggled.

## Evidence
- gpui-component 0.7.0 `src/theme/mod.rs` (`Theme::edit`, `apply_config`), `src/theme/registry.rs:41-73` (registry observer), `src/title_bar.rs`, `src/root.rs:448-458` (window border) read 2026-09-28
- https://gpui-kit.com/docs/i18n.md (rust-i18n ≥ 4.2, `set_locale`), https://gpui-kit.com/component/title-bar.md, https://gpui-kit.com/component/settings.md (2026-09-28)
- `cargo nextest run -p dial-ui` 10/10 incl. `style_overrides_survive_mode_switches_and_registry_reloads`; `cargo ci` green (2026-09-28)
