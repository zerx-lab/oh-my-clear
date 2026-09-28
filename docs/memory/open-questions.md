# Open questions
<!-- Undecided development questions only (no progress, plans or next steps). Delete a line once an ADR or lesson answers it. Template: skill://memory -->
- Does "drop alacritty_terminal" also exclude its `tty` module (ADR 0010 uses it for PTYs)? — options: keep `tty` only / own PTY code (rustix openpty + ConPTY) inside an unsafe island — owner: user — since 2026-09-28
- Node ≥ 22 for npx-distributed ACP agents — options: bundled managed Node / required on PATH — owner: user — since 2026-09-28
- Windows MSVC libghostty-vt link is proven only by upstream/third-party CI; aarch64-windows unproven — options: confirm on dial's Windows CI / drop aarch64-windows — owner: agent — since 2026-09-28
- Transcript streaming coalescing: one `notify` per frame (≤33 ms, ADR 0011) vs the earlier ~100 ms — confirm against a real transcript view — owner: agent — since 2026-09-28
- Unused-dependency tool: cargo-shear proposed, not installed — owner: user — since 2026-09-28
