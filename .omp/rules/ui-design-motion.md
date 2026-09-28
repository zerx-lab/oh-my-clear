---
description: oh-my-clear UI design + motion rules (tokens, Apple-style springs, reduced motion, frame budgets) — read before writing or reviewing UI code
globs: ["crates/omc-ui/**/*.rs", "apps/oh-my-clear/**/*.rs"]
---

# oh-my-clear UI design & motion (ADR 0011)

The UI is the product's first principle: quiet, dense, keyboard-first; content opaque and calm, chrome light; one accent; motion is feedback, never decoration.

## Tokens only
- Colors, spacing, radii, shadows, font sizes, durations and springs come from `omc_ui::tokens` / the active theme. No literal `rgb(..)`/`hsla(..)`, ad-hoc `px(..)` sizes, or `Duration::from_millis` in views (exceptions: 0 and 1 px hairlines).
- Palettes are gpui-component `ThemeSet` JSON presets in `crates/omc-ui/themes/`, read through `cx.theme()`; user overrides go through `omc_ui::theme::ThemeStyle` and `UiSettings::update` (ADR 0013). Colours the app authors itself (accent ramps) are OKLCH, converted to `Hsla` once when the theme is applied — never in `render`. Body text ≥ 4.5:1, secondary ≥ 3:1.
- Fonts: bundled Inter (UI) + JetBrains Mono (code/monospace), registered with `text_system().add_fonts(..)?` before the first window; name families explicitly (GPUI panics on a missing family). Body 13/18; weights 400/500/600 only.
- 4 px grid (2,4,6,8,12,16,24,32); radii 4/6/8/12 (nested = outer − padding); three elevations; Lucide icons 16 px / 1.5 stroke.

## Motion = springs from the preset table
- Anything the user can re-trigger mid-flight (open/close, select, reorder, resize, show/hide) uses a spring (`gpui_kit::base::spring`, `.with_spring(..)`, or `omc_ui::motion::AnimatedValue`), never a duration curve.
- Only the `const` presets `MICRO, UI (default), SNAPPY, PANEL, SMOOTH, TRACK, THROWN`, in Apple's parameterisation: perceptual duration + bounce → `Spring::new(d).with_damping(1.0 - bounce)`.
- Bounce > 0 only when a gesture's release velocity points toward the target; never above 0.3.
- Retarget, never restart: keep position and velocity; seed from gesture velocity (`AnimatedValue::release(target, v)`). Drags track 1:1; the spring resumes on release.
- Exits run at ~0.7× enter duration; dismiss is never slower than present.
- Duration animations only for loops and one-shot decoration; decorative loops `.with_max_fps(30.)`.
- Motion state that must survive unmount, virtualization or a skipped frame lives in the view entity (`AnimatedValue`), not element-keyed state. Animation clocks use `cx.background_executor().now()`.

## Don't animate
- Layout properties (`w/h/top/left/margin/padding/gap`, flex changes). Animate `opacity` and paint offset (`omc_ui::motion::Offset` / `with_element_offset`); size changes use `MotionReveal`, clipped, once.
- Typed text, selections, scrolling content, re-sorts of >50 rows, anything while idle.
- Travel stays short (4–8 px enter/exit offsets); no scale on text-bearing surfaces; no blur transitions.

## Reduced motion / accessibility
- Read motion through `MotionPolicy` (backed by `cx.reduce_motion()`); re-apply the OS setting on window activation (`apply_system_reduce_motion`).
- Reduced = no translate/scale/bounce/parallax/loops; replace movement with a ≤150 ms opacity fade; gestures stay 1:1; cursor blink off.
- Reduce Transparency → opaque window and surfaces. Increase Contrast → contrast token up.

## Frame budget (60/120 Hz)
- While anything animates: main-thread render+layout+paint ≤ 4 ms/frame at 120 Hz, ≤ 8 ms at 60 Hz. Profile (gpui-kit `profiler` feature) before merging new motion.
- `window.request_animation_frame()` re-renders the whole current view: isolate animated parts in small child entities; stop requesting frames the moment values settle.
- Streaming: at most one `cx.notify()` per frame per view (≤ 33 ms coalescing); parsing via `background_spawn`.
- Input → first visual response on the next frame; no artificial delays except tooltips.

## Interaction & platform
- Every command is an `Action`, listed in the command palette with its key binding; shortcut hints visible in menus/tooltips.
- Every focusable element shows a 2 px `focus_visible` ring; never remove focus styling. Hit targets ≥ 24×24 px.
- Materials: opaque by default; translucency only on chrome (sidebar/titlebar/palette scrim): macOS `Blurred`, Windows 11 ≥ 22621 `MicaBackdrop`, Linux Opaque. Never translucency under body text without an opaque fallback.
- Titlebars: gpui-component `TitleBar`; macOS transparent titlebar with traffic lights, Windows `window_control_area(Max)` on maximize (snap layouts), Linux client-side decorations + resize edges.
