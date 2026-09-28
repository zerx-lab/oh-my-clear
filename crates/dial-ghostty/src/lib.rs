//! Safe wrapper over libghostty-vt, Ghostty's VT emulator library (C ABI, built with
//! zig from the pinned `third_party/ghostty` submodule and statically linked).
//!
//! This is the workspace's only crate that contains `unsafe` code (FFI). Every other
//! crate keeps `unsafe_code = "forbid"`. Internal deps: none.
//! Architecture: docs/memory/decisions/0010-terminal-libghostty-vt-unsafe-island.md; research: `docs/research/2026-09-28-libghostty-vt.md`.
//!
//! # Build requirements (all platforms)
//! - zig 0.16.x on `PATH`, or `$ZIG` pointing at it. Ghostty accepts only this
//!   major.minor.
//! - The submodule, from the repo root: `git submodule update --init --depth 1 third_party/ghostty`
//! - One-time Zig package fetch from the repo root (needs network, about 108 MB
//!   download; extracts into the git-ignored `third_party/ghostty/zig-pkg/`):
//!   `zig build --build-file third_party/ghostty/build.zig --fetch=all`
//! - Windows: build on a Windows host with the MSVC Build Tools and Windows SDK.
//!   zig cannot cross-compile the MSVC-ABI archive from macOS or Linux.
//!
//! The build script never touches the network. It resolves packages only from
//! `zig-pkg/`. It keeps the zig cache in `<target-dir>/ghostty-zig-cache`, which is
//! shared by all profiles, and refuses a submodule HEAD other than the commit the
//! checked-in bindings were generated from.
//!
//! # Threading
//! [`Terminal`] is `Send` but not `Sync`. The daemon gives each terminal to the
//! thread that owns its PTY.

#[expect(
    unsafe_code,
    dead_code,
    non_upper_case_globals,
    unreachable_pub,
    clippy::unreadable_literal,
    reason = "verbatim bindgen output: C naming, unused enum constants, generated literals"
)]
mod ffi;

mod error;
#[expect(unsafe_code, reason = "FFI to libghostty-vt: snapshot encode/decode")]
mod snapshot;
#[expect(
    unsafe_code,
    reason = "FFI to libghostty-vt: terminal lifecycle, queries, formatter"
)]
mod terminal;

pub use error::{Error, GhosttyError, Result};
pub use snapshot::Snapshot;
pub use terminal::{CursorPosition, FormatKind, Region, Size, Terminal};

#[cfg(test)]
mod tests {
    use super::{Error, FormatKind, GhosttyError, Region, Size, Snapshot, Terminal};

    const HELLO_RED: &[u8] = b"hello \x1b[31mred\x1b[0m";

    /// `None` only after the assertion has already failed the test.
    fn terminal(cols: u16, rows: u16) -> Option<Terminal> {
        let created = Terminal::new(cols, rows, 1000);
        assert!(
            created.is_ok(),
            "terminal {cols}x{rows} is created: {created:?}"
        );
        created.ok()
    }

    fn text(terminal: &Terminal, kind: FormatKind, region: Region) -> String {
        let formatted = terminal.format(kind, region);
        assert!(
            formatted.is_ok(),
            "format {kind:?}/{region:?} succeeds: {formatted:?}"
        );
        String::from_utf8_lossy(&formatted.unwrap_or_default()).into_owned()
    }

    #[test]
    fn plain_format_strips_styles_and_vt_format_keeps_them() {
        let Some(mut term) = terminal(20, 3) else {
            return;
        };
        term.feed(HELLO_RED);

        let plain = text(&term, FormatKind::Plain, Region::Screen);
        let vt = text(&term, FormatKind::Vt, Region::Screen);

        assert_eq!(plain, "hello red", "plain text has no escapes");
        assert!(
            vt.contains("\x1b[38;5;1mred"),
            "VT keeps the red SGR: {vt:?}"
        );
    }

    #[test]
    fn escape_sequence_split_across_feeds_is_parsed() {
        let Some(mut term) = terminal(20, 3) else {
            return;
        };
        let (first, second) = HELLO_RED.split_at(8); // splits inside ESC [ 3 1 m
        term.feed(first);
        term.feed(second);

        assert_eq!(
            text(&term, FormatKind::Plain, Region::Screen),
            "hello red",
            "a split SGR is not printed as text"
        );
    }

    #[test]
    fn vt_output_replays_into_an_equal_screen() {
        let Some(mut source) = terminal(20, 3) else {
            return;
        };
        source.feed(b"\x1b[1mbold\x1b[0m plain\r\nline two");
        let vt = source.format(FormatKind::Vt, Region::ScreenAndScrollback);
        assert!(vt.is_ok(), "VT format succeeds: {vt:?}");

        let Some(mut replay) = terminal(20, 3) else {
            return;
        };
        replay.feed(&vt.unwrap_or_default());

        assert_eq!(
            text(&replay, FormatKind::Vt, Region::Screen),
            text(&source, FormatKind::Vt, Region::Screen),
            "replayed VT reproduces styles and content"
        );
        assert_eq!(
            replay.cursor().ok(),
            source.cursor().ok(),
            "cursor is restored"
        );
    }

    #[test]
    fn screen_region_excludes_scrollback_and_full_region_includes_it() {
        let Some(mut term) = terminal(10, 2) else {
            return;
        };
        term.feed(b"one\r\ntwo\r\nthree\r\nfour");

        let screen = text(&term, FormatKind::Plain, Region::Screen);
        let all = text(&term, FormatKind::Plain, Region::ScreenAndScrollback);

        assert_eq!(screen, "three\nfour", "only the active rows");
        assert_eq!(
            all, "one\ntwo\nthree\nfour",
            "history first, then the screen"
        );
        assert_eq!(
            term.scrollback_rows().ok(),
            Some(2),
            "two rows scrolled off"
        );
    }

    #[test]
    fn zero_scrollback_keeps_no_history() {
        let created = Terminal::new(10, 2, 0);
        assert!(created.is_ok(), "zero scrollback is valid: {created:?}");
        let Ok(mut term) = created else { return };
        term.feed(b"one\r\ntwo\r\nthree\r\nfour");

        assert_eq!(term.scrollback_rows().ok(), Some(0), "history is dropped");
        assert_eq!(
            text(&term, FormatKind::Plain, Region::ScreenAndScrollback),
            "three\nfour",
            "only the screen remains"
        );
    }
    #[test]
    fn scrollback_limit_bounds_history_and_zero_then_nonzero_restores_it() {
        let created = Terminal::new(10, 2, 50);
        assert!(created.is_ok(), "terminal is created: {created:?}");
        let Ok(mut term) = created else { return };
        for _ in 0..5000 {
            term.feed(b"line\r\n");
        }
        let kept = term.scrollback_rows().unwrap_or_default();
        assert!(
            (50..5000).contains(&kept),
            "history is pruned near 50 lines, kept {kept}"
        );

        let disabled = term.set_scrollback_lines(0);
        assert!(disabled.is_ok(), "disabling succeeds: {disabled:?}");
        assert_eq!(
            term.scrollback_rows().ok(),
            Some(0),
            "disabling erases history"
        );

        let enabled = term.set_scrollback_lines(50);
        assert!(enabled.is_ok(), "re-enabling succeeds: {enabled:?}");
        term.feed(b"a\r\nb\r\nc\r\n");
        assert!(
            term.scrollback_rows().is_ok_and(|rows| rows > 0),
            "history is kept again after re-enabling"
        );
    }

    #[test]
    fn cursor_tracks_output() {
        let Some(mut term) = terminal(20, 3) else {
            return;
        };
        term.feed(HELLO_RED);
        let cursor = term.cursor().ok().map(|c| (c.x, c.y));
        assert_eq!(cursor, Some((9, 0)), "cursor sits after `hello red`");

        term.feed(b"\r\n");
        let cursor = term.cursor().ok().map(|c| (c.x, c.y));
        assert_eq!(cursor, Some((0, 1)), "CR LF moves to the next row start");
    }

    #[test]
    fn resize_changes_size_and_reflows() {
        let Some(mut term) = terminal(10, 3) else {
            return;
        };
        term.feed(b"abcdefghij");

        let resized = term.resize(5, 4, 8, 16);
        assert!(resized.is_ok(), "resize succeeds: {resized:?}");

        assert_eq!(
            term.size().ok(),
            Some(Size { cols: 5, rows: 4 }),
            "new size"
        );
        assert_eq!(
            text(&term, FormatKind::Plain, Region::Screen),
            "abcde\nfghij",
            "the soft-wrapped line reflows onto two rows"
        );
    }

    #[test]
    fn zero_sized_terminal_is_rejected_without_panicking() {
        assert!(
            matches!(
                Terminal::new(0, 24, 100),
                Err(Error::InvalidSize { cols: 0, rows: 24 })
            ),
            "zero columns are rejected"
        );
        assert!(
            matches!(
                Terminal::new(80, 0, 100),
                Err(Error::InvalidSize { cols: 80, rows: 0 })
            ),
            "zero rows are rejected"
        );

        let Some(mut term) = terminal(10, 3) else {
            return;
        };
        assert!(
            matches!(term.resize(0, 0, 0, 0), Err(Error::InvalidSize { .. })),
            "resize to zero is rejected"
        );
        assert_eq!(
            term.size().ok(),
            Some(Size { cols: 10, rows: 3 }),
            "size is unchanged"
        );
    }

    #[test]
    fn osc_title_pwd_and_prompt_marks_are_tracked() {
        let Some(mut term) = terminal(40, 3) else {
            return;
        };
        assert_eq!(term.title().ok(), Some(None), "no title before OSC 2");
        assert_eq!(term.pwd().ok(), Some(None), "no pwd before OSC 7");
        assert_eq!(
            term.cursor_at_prompt().ok(),
            Some(false),
            "no prompt mark yet"
        );

        term.feed(b"\x1b]2;build logs\x07\x1b]7;file://host/tmp/work\x1b\\\x1b]133;A\x07$ ");

        assert_eq!(
            term.title().ok().flatten().as_deref(),
            Some("build logs"),
            "OSC 2 sets the title"
        );
        assert_eq!(
            term.pwd().ok().flatten().as_deref(),
            Some("file://host/tmp/work"),
            "OSC 7 (ST-terminated) sets the pwd"
        );
        assert_eq!(
            term.cursor_at_prompt().ok(),
            Some(true),
            "OSC 133;A marks a prompt"
        );
    }

    #[test]
    fn snapshot_round_trip_restores_screen_history_and_state() {
        let Some(mut term) = terminal(20, 2) else {
            return;
        };
        term.feed(b"\x1b]2;title\x07\x1b]7;file://h/repo\x07first\r\nsecond\r\n");
        term.feed(HELLO_RED);

        let snapshot = term.snapshot();
        assert!(snapshot.is_ok(), "encode succeeds: {snapshot:?}");
        let Ok(snapshot) = snapshot else { return };
        assert!(
            snapshot.as_bytes().starts_with(b"GHOSTSNP"),
            "snapshot has its magic header"
        );

        let wire = Snapshot::from_bytes(snapshot.into_bytes());
        let restored = wire.restore();
        assert!(restored.is_ok(), "decode succeeds: {restored:?}");
        let Ok(restored) = restored else { return };

        for (kind, region) in [
            (FormatKind::Vt, Region::ScreenAndScrollback),
            (FormatKind::Plain, Region::ScreenAndScrollback),
        ] {
            assert_eq!(
                text(&restored, kind, region),
                text(&term, kind, region),
                "{kind:?} output matches after restore"
            );
        }
        assert_eq!(
            restored.cursor().ok(),
            term.cursor().ok(),
            "cursor restored"
        );
        assert_eq!(restored.title().ok(), term.title().ok(), "title restored");
        assert_eq!(restored.pwd().ok(), term.pwd().ok(), "pwd restored");
    }

    #[test]
    fn malformed_snapshots_are_errors() {
        for bytes in [
            Vec::new(),
            b"GHOSTSNP".to_vec(),
            b"not a snapshot at all".to_vec(),
        ] {
            let result = Snapshot::from_bytes(bytes.clone()).restore();
            assert!(
                matches!(
                    result,
                    Err(Error::Ghostty {
                        code: GhosttyError::InvalidValue,
                        ..
                    })
                ),
                "{bytes:?} is rejected as invalid: {result:?}"
            );
        }

        let Some(mut term) = terminal(10, 2) else {
            return;
        };
        term.feed(b"text");
        let encoded = term
            .snapshot()
            .map(Snapshot::into_bytes)
            .unwrap_or_default();
        let truncated = encoded
            .get(..encoded.len().saturating_sub(4))
            .unwrap_or_default();
        let result = Snapshot::from_bytes(truncated.to_vec()).restore();
        assert!(
            result.is_err(),
            "a truncated snapshot is rejected: {result:?}"
        );
    }

    #[test]
    fn terminal_moves_to_another_thread() {
        let Some(mut term) = terminal(20, 2) else {
            return;
        };
        term.feed(b"moved");
        let joined = std::thread::spawn(move || {
            term.feed(b" across");
            term.format(FormatKind::Plain, Region::Screen)
        })
        .join();

        assert!(
            matches!(&joined, Ok(Ok(bytes)) if bytes.as_slice() == b"moved across"),
            "the terminal keeps working on the new thread: {joined:?}"
        );
    }
}
