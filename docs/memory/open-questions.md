# Open questions
<!-- Undecided development questions only (no progress or plans). Delete a line once an ADR or lesson answers it. Template: skill://memory -->
- Unused-dependency tool: cargo-shear proposed, not installed — owner: user — since 2026-09-28
- Where do UI preferences (`UiSettings`: theme, style, language) persist? — options: GUI-local file in the platform config dir (toml, needs a config-dir helper such as `dirs`) / daemon-owned config over IPC / both (UI caches) — owner: user — since 2026-09-28
