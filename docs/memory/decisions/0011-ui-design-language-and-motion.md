---
status: accepted
date: 2026-09-28
tags: [gui, design, motion, accessibility, platform]
---
# 0011 UI first: quiet keyboard-first design language, Apple-style spring motion on gpui-kit primitives

## Context and Problem Statement
The user's first principle is that the UI is refined and beautiful, holds up aesthetically for five years, and follows Apple's motion principles with excellent interaction, on macOS, Windows and Linux. oh-my-clear needs a design system and a motion model that are enforceable in gpui-kit 0.7.0 code without panics or `unsafe`.

## Considered Options
* Use gpui-component defaults (duration curves; enter-only dialog/sheet/popover animations)
* Write our own spring solver and animation system
* Build on the springs gpui already ships (`gpui-pre` `SpringConfig`, gpui-base `Spring` in the Apple parameterisation) and add a small `omc_ui::motion` layer (**chosen**)

## Decision Outcome
- **Motion model (Apple WWDC18 803 / WWDC23 10158):**
  - Springs are specified by perceptual duration `d` plus bounce `b`: `Spring::new(d).with_damping(1 − b)`, giving k = (2π/d)², c = 4π(1−b)/d.
  - The only presets allowed are `const` items: `MICRO 0.20/0`, `UI 0.35/0` (the default), `SNAPPY 0.30/0.15`, `PANEL 0.45/0.08`, `SMOOTH 0.50/0`, `TRACK 0.15/0.14`, `THROWN 0.45/0.20`. Because they are `const`, an invalid literal is a compile error, not a runtime panic.
  - Bounce must be 0 unless a gesture releases toward the target, and is never above 0.3.
  - Anything re-triggerable uses a spring that is **retargeted rather than restarted**, keeping velocity.
  - Drags track the pointer 1:1.
  - An exit runs at ≈0.7× the enter duration.
  - Duration animations are only for loops and one-shot decoration; decorative loops are capped at 30 fps.
- **Additions in `omc_ui::motion`:**
  - `AnimatedValue`, owned by an entity: survives unmount/virtualization and accepts a release velocity from a 100 ms `VelocityTracker`.
  - A paint-only `Offset` element (`with_element_offset`), so nothing animates layout.
  - `MotionPolicy`: under Reduce Motion, movement becomes a ≤150 ms opacity fade instead of snapping. The OS setting is re-applied on window activation, because gpui-base reads it only at init on macOS/Windows.
  - A `SystemPrefs` global (accent, reduce transparency, increase contrast), read through safe APIs in `objc2-app-kit` / `windows` / `ashpd`, which are already in the gpui graph.
- **Frame budget:**
  - Measured at 120 Hz on ProMotion. While anything animates, main-thread render + layout + paint must stay ≤ 4 ms per frame (≤ 8 ms at 60 Hz).
  - Animated regions are small child entities, because `request_animation_frame` re-renders the whole view.
  - Stop requesting frames once motion settles.
  - Streaming text gets at most one `notify` per frame, capped at 33 ms. This **replaces** the earlier ~100 ms coalescing, which showed as 10 Hz steps.
- **Design language:** a quiet, dense instrument, not a website. Content is opaque and calm; chrome is light. One accent color. Motion is feedback, never decoration.
  - Colors: tokens authored in OKLCH and generated from `base_hue`, `accent` and `contrast`, converted to `Hsla` once at theme load. The conversion is about 25 lines in omc-ui; no color crate. Body text contrast ≥ 4.5:1.
  - Fonts: **Inter 4.1** for UI and **JetBrains Mono 2.304** for code and monospace text. Both are OFL, bundled unmodified, and registered before the first window, because GPUI panics on a missing family.
  - Scale: body 13/18. Weights 400/500/600 only.
  - Layout: 4 px grid. Radii 4/6/8/12. Three elevations. Lucide icons at 16 px / 1.5 stroke.
  - Interaction: keyboard-first. Every command is an `Action` in the command palette. `focus_visible` rings are always on. Hit targets ≥ 24 px. Input shows a visible response on the next frame.
- **Materials:** Opaque by default. Translucent chrome is opt-in: macOS `Blurred`, Windows 11 ≥ 22621 `MicaBackdrop`, Linux Opaque (only KDE blurs). No native Liquid Glass, since it would need `unsafe` on the raw window; approximate it with translucent fills, hairline highlights and shadows.
- **Titlebars:** client-side via gpui-component `TitleBar`: macOS transparent titlebar, Windows `window_control_area(Max)` for snap layouts, Linux CSD.
- **Enforcement:** rulebook rule `rule://ui-design-motion` (tokens only, preset springs, no layout animation, reduced motion, frame budgets). `rule://gpui-patterns` points to it.

### Consequences
* Good: Apple-grade motion without a custom solver. One source of tokens. Accessibility handled by construction. Identical typography and density on all three OSes.
* Bad: gpui-kit's motion APIs are young and change weekly, so they are wrapped behind `omc_ui::motion` and kept on the `=0.7.0` pin. The built-in Dialog/Sheet/Popover have no exit motion until the app replaces them. High refresh rates on Windows/Linux, and Mica's appearance, are unverified.

## Evidence
- docs/research/2026-09-28-ui-motion.md: Apple sources mapped to numbers; gpui-pre 0.3.7 `src/spring.rs`, gpui-base 0.7.0 `src/motion.rs:442-555`, `reduce_motion.rs`, and `window.rs:2622`, read in source; a 120 Hz probe measuring an 8.33 ms median frame (2026-09-28)
