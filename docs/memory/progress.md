# Progress

## Milestones
- [x] M0 Repo scaffolding (gates, omp rules/TTSR/skills, memory) — done 2026-09-28
- [ ] M1 gpui-kit shell — acceptance: `dial` opens a themed window (tokens, bundled fonts, spring presets, reduced-motion policy) with sidebar + empty session pane on macOS/Windows/Linux; all gates green on the 3-OS CI
- [ ] M1.5 Daemon + IPC — acceptance: `dial` auto-spawns/attaches `dial-daemon` over UDS/named pipe with token auth; quitting the UI leaves the daemon running; relaunch reattaches (epoch/seq resync); `dial-daemon status|stop`
- [ ] M2 Sessions — acceptance: native agent (Anthropic + OpenAI-compatible) and one ACP agent (registry install) each run a prompt in a git worktree from the daemon; transcript + tool calls stream to the UI; cancel and permission prompts survive a UI restart; terminal pane renders a daemon PTY via libghostty-vt
- [ ] M3 Multi-agent orchestration — acceptance: Run/Task/Dispatch model, parallel agents in separate worktrees, OrchestratorPort (native) + daemon MCP (ACP agents) for inter-agent messages, diff review

## Known issues
- (none)

## Session log (last 10, newest first)
- 2026-09-28 main: architecture v2 — ADRs 0008–0011 (daemon/IPC, native agent + ACP, libghostty-vt unsafe island, UI/motion), research ×5, crates ipc/llm/native/telemetry/ghostty + apps/dial-daemon, `xtask layers`, 3-OS CI, new rules, release split-debuginfo · verified by `cargo ci` green (layers + 23 tests incl. 13 dial-ghostty), `omp ttsr test` per new rule, daemon logs on stderr only · next: M1 window, M1.5 daemon/IPC
- 2026-09-28 main: virtual multi-crate workspace + xtask + dev profiles + tracing/panic hook + Zed wiring (ADR 0007) · verified by `cargo ci` green, `cargo dial`, bad RUST_LOG exit 1, `cargo new-crate probe` smoke, panic-hook/filter throwaway · next: M1
- 2026-09-28 main: bootstrap scaffolding + research (gpui-kit, Zeron/Orca, gates, memory) · verified by fmt/clippy/nextest(--no-tests=warn)/deny + `omp ttsr test` per rule · next: M1
