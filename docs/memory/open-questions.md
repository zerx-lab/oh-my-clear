# Open questions
<!-- Undecided development questions only (no progress or plans). Delete a line once an ADR or lesson answers it. Template: skill://memory -->
- Unused-dependency tool: cargo-shear proposed, not installed — owner: user — since 2026-09-28
- Should the wire protocol express Linux per-app state outside files (dconf subtrees, Flatpak permission store) for uninstall? — options: `SpecialAction::ResetDconf{path}` / `ResetFlatpakPermissions{app_id}` / keep them out of scope — owner: agent — since 2026-09-28
- Should the macOS uninstaller use Homebrew's curated cask `zap` lists (formulae.brew.sh API, 4.7k casks) for apps not installed via brew? — options: bundled snapshot / opt-in download setting / no — owner: user — since 2026-09-28
