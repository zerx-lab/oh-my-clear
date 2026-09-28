//! Safe owner of a libghostty-vt terminal: feed, resize, query, format.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::size_of;
use std::ptr::{self, NonNull};
use std::slice;

use crate::error::{Error, Result, check};
use crate::ffi;

/// Terminal grid size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Columns (width in cells).
    pub cols: u16,
    /// Rows (height in cells).
    pub rows: u16,
}

/// Cursor position in the active area, zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    /// Column.
    pub x: u16,
    /// Row.
    pub y: u16,
}

/// Output encoding for [`Terminal::format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    /// Text only, trailing whitespace trimmed. For agents reading the screen.
    Plain,
    /// VT sequences that recreate the content plus terminal state (styles, cursor,
    /// modes, palette, tabstops, scrolling region, OSC 7 pwd, keyboard modes,
    /// hyperlinks, charsets). Feeding it to a fresh terminal reproduces the screen.
    Vt,
}

/// Which rows [`Terminal::format`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Only the active area (the bottom `rows` rows, what a user sees unscrolled).
    Screen,
    /// Scrollback followed by the active area.
    ScreenAndScrollback,
}

/// A VT terminal emulator: parser plus screen, scrollback, and state.
///
/// Thread confinement: the type is `Send` but not `Sync`. Move it to the thread that
/// owns the PTY and never share it; libghostty-vt objects are not thread-safe.
#[derive(Debug)]
pub struct Terminal {
    raw: NonNull<ffi::GhosttyTerminalImpl>,
}

// SAFETY: moving a terminal to another thread is sound because (verified against the
// pinned Ghostty source): the handle owns all of its state; the default allocator is
// libc malloc (`src/lib/allocator.zig`, libc is linked because SIMD is enabled), which
// frees correctly from any thread; lib-vt keeps no thread-local state (no
// `threadlocal` in `src/terminal` or `src/lib`) and its `std.Io` (TinyIo) is
// stateless; and this wrapper registers no callbacks or userdata pointers. `Sync` is
// deliberately not implemented: concurrent access is not allowed by the C API.
unsafe impl Send for Terminal {}

impl Terminal {
    /// Create a terminal of `cols` x `rows` cells that keeps roughly
    /// `scrollback_lines` lines of history (libghostty prunes whole pages, so it
    /// usually keeps somewhat more; `0` keeps none).
    ///
    /// # Errors
    /// [`Error::InvalidSize`] if either dimension is zero; [`Error::Ghostty`] if
    /// libghostty-vt fails (e.g. out of memory).
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Result<Self> {
        validate_size(cols, rows)?;
        let mut raw: ffi::GhosttyTerminal = ptr::null_mut();
        // SAFETY: a null allocator selects the default one; `raw` is a valid out-pointer.
        let code = unsafe { ffi::ghostty_terminal_new(ptr::null(), &raw mut raw, cols, rows) };
        check("terminal_new", code)?;
        let raw = NonNull::new(raw).ok_or(Error::NullHandle("terminal_new"))?;
        let mut terminal = Self { raw };
        terminal.set_scrollback_lines(scrollback_lines)?;
        Ok(terminal)
    }

    /// Wrap a handle returned by libghostty-vt; the wrapper takes ownership.
    pub(crate) fn from_raw(raw: ffi::GhosttyTerminal, operation: &'static str) -> Result<Self> {
        let raw = NonNull::new(raw).ok_or(Error::NullHandle(operation))?;
        Ok(Self { raw })
    }

    pub(crate) fn as_raw(&self) -> ffi::GhosttyTerminal {
        self.raw.as_ptr()
    }

    /// Set the approximate scrollback limit in lines. `0` disables scrollback and
    /// erases retained history. A non-zero limit is the only bound: libghostty's
    /// separate byte limit is removed, and pruning is page-granular, so it usually
    /// keeps somewhat more lines than requested.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if libghostty-vt rejects the option.
    pub fn set_scrollback_lines(&mut self, lines: usize) -> Result<()> {
        if lines == 0 {
            // The line limit is page-granular and would keep one page; a zero byte
            // limit is documented to disable scrollback outright.
            return self.set_usize_option(
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
                Some(0),
                "terminal_set(SCROLLBACK_MAX_BYTES)",
            );
        }
        self.set_usize_option(
            ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
            None,
            "terminal_set(SCROLLBACK_MAX_BYTES)",
        )?;
        self.set_usize_option(
            ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,
            Some(lines),
            "terminal_set(SCROLLBACK_MAX_LINES)",
        )
    }

    /// Set a `size_t*` option; `None` passes NULL (documented as "remove the limit"
    /// for the scrollback options).
    fn set_usize_option(
        &mut self,
        option: ffi::GhosttyTerminalOption,
        value: Option<usize>,
        operation: &'static str,
    ) -> Result<()> {
        let pointer = value
            .as_ref()
            .map_or(ptr::null(), |value| ptr::from_ref(value).cast::<c_void>());
        // SAFETY: the handle is live and exclusively borrowed; callers pass only
        // options documented to read a `size_t` (or accept NULL), and `value`
        // outlives the call.
        let code = unsafe { ffi::ghostty_terminal_set(self.as_raw(), option, pointer) };
        check(operation, code)
    }

    /// Feed raw bytes from the PTY. Escape sequences may be split across calls.
    pub fn feed(&mut self, bytes: &[u8]) {
        // SAFETY: the handle is live and exclusively borrowed; `bytes` is valid for
        // `bytes.len()` reads for the duration of the call.
        unsafe { ffi::ghostty_terminal_vt_write(self.as_raw(), bytes.as_ptr(), bytes.len()) };
    }

    /// Resize the grid (reflowing the primary screen). The cell pixel size feeds
    /// size reports and image protocols; pass `0` if unknown.
    ///
    /// # Errors
    /// [`Error::InvalidSize`] if either dimension is zero; [`Error::Ghostty`] on failure.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<()> {
        validate_size(cols, rows)?;
        // SAFETY: the handle is live and exclusively borrowed; all other arguments are
        // plain integers.
        let code = unsafe {
            ffi::ghostty_terminal_resize(self.as_raw(), cols, rows, cell_width_px, cell_height_px)
        };
        check("terminal_resize", code)
    }

    /// Current grid size.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if the query fails.
    pub fn size(&self) -> Result<Size> {
        // SAFETY: COLS and ROWS are documented to write a `uint16_t`.
        let cols = unsafe { self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_COLS) }?;
        // SAFETY: as above.
        let rows = unsafe { self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_ROWS) }?;
        Ok(Size { cols, rows })
    }

    /// Cursor position in the active area.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if the query fails.
    pub fn cursor(&self) -> Result<CursorPosition> {
        // SAFETY: CURSOR_X and CURSOR_Y are documented to write a `uint16_t`.
        let x =
            unsafe { self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CURSOR_X) }?;
        // SAFETY: as above.
        let y =
            unsafe { self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CURSOR_Y) }?;
        Ok(CursorPosition { x, y })
    }

    /// Window title set by OSC 0/2, or `None` if never set. Invalid UTF-8 is replaced.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if the query fails.
    pub fn title(&self) -> Result<Option<String>> {
        self.get_string(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_TITLE)
    }

    /// Working directory reported by OSC 7 (usually a `file://host/path` URL), or
    /// `None` if never reported. Invalid UTF-8 is replaced.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if the query fails.
    pub fn pwd(&self) -> Result<Option<String>> {
        self.get_string(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_PWD)
    }

    /// Whether the cursor sits in a shell prompt, per OSC 133 shell-integration marks.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if the query fails.
    pub fn cursor_at_prompt(&self) -> Result<bool> {
        // SAFETY: CURSOR_AT_PROMPT is documented to write a `bool`.
        unsafe { self.get::<bool>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CURSOR_AT_PROMPT) }
    }

    /// Number of rows currently held in scrollback.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if the query fails.
    pub fn scrollback_rows(&self) -> Result<usize> {
        // SAFETY: SCROLLBACK_ROWS is documented to write a `size_t`.
        unsafe { self.get::<usize>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS) }
    }

    /// Serialize the active screen (and optionally scrollback) as UTF-8 text or VT.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if libghostty-vt fails to build or run the formatter.
    pub fn format(&self, kind: FormatKind, region: Region) -> Result<Vec<u8>> {
        let selection = match region {
            Region::Screen => Some(self.active_area_selection()?),
            Region::ScreenAndScrollback => None,
        };
        let vt = kind == FormatKind::Vt;
        let options = ffi::GhosttyFormatterTerminalOptions {
            size: size_of::<ffi::GhosttyFormatterTerminalOptions>(),
            emit: if vt {
                ffi::GhosttyFormatterFormat_GHOSTTY_FORMATTER_FORMAT_VT
            } else {
                ffi::GhosttyFormatterFormat_GHOSTTY_FORMATTER_FORMAT_PLAIN
            },
            unwrap: false,
            // Trailing blanks can carry background colour in VT output; keep them there.
            trim: !vt,
            extra: ffi::GhosttyFormatterTerminalExtra {
                size: size_of::<ffi::GhosttyFormatterTerminalExtra>(),
                palette: vt,
                modes: vt,
                scrolling_region: vt,
                tabstops: vt,
                pwd: vt,
                keyboard: vt,
                screen: ffi::GhosttyFormatterScreenExtra {
                    size: size_of::<ffi::GhosttyFormatterScreenExtra>(),
                    cursor: vt,
                    style: vt,
                    hyperlink: vt,
                    protection: vt,
                    kitty_keyboard: vt,
                    charsets: vt,
                },
            },
            selection: selection.as_ref().map_or(ptr::null(), ptr::from_ref),
        };
        let formatter = Formatter::new(self, options)?;
        formatter.format()
    }

    /// Linear selection from the first to the last cell of the active area.
    fn active_area_selection(&self) -> Result<ffi::GhosttySelection> {
        let size = self.size()?;
        let start = self.grid_ref(0, 0)?;
        let end = self.grid_ref(
            size.cols.saturating_sub(1),
            u32::from(size.rows.saturating_sub(1)),
        )?;
        Ok(ffi::GhosttySelection {
            size: size_of::<ffi::GhosttySelection>(),
            start,
            end,
            rectangle: false,
        })
    }

    /// Untracked grid reference for an active-area point. Valid until the next mutation.
    fn grid_ref(&self, x: u16, y: u32) -> Result<ffi::GhosttyGridRef> {
        let point = ffi::GhosttyPoint {
            tag: ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_ACTIVE,
            value: ffi::GhosttyPointValue {
                coordinate: ffi::GhosttyPointCoordinate { x, y },
            },
        };
        let mut out = ffi::GhosttyGridRef {
            size: size_of::<ffi::GhosttyGridRef>(),
            node: ptr::null_mut(),
            x: 0,
            y: 0,
        };
        // SAFETY: the handle is live; `out` is a valid, correctly sized out-pointer.
        let code = unsafe { ffi::ghostty_terminal_grid_ref(self.as_raw(), point, &raw mut out) };
        check("terminal_grid_ref", code)?;
        Ok(out)
    }

    /// Borrowed-string query copied into an owned `String`; empty means unset.
    fn get_string(&self, data: ffi::GhosttyTerminalData) -> Result<Option<String>> {
        // SAFETY: callers pass only TITLE/PWD, documented to write a `GhosttyString`.
        let borrowed = unsafe { self.get::<ffi::GhosttyString>(data) }?;
        if borrowed.ptr.is_null() || borrowed.len == 0 {
            return Ok(None);
        }
        // SAFETY: libghostty guarantees `ptr` is valid for `len` bytes until the next
        // mutating call; `&self` prevents mutation while we copy.
        let bytes = unsafe { slice::from_raw_parts(borrowed.ptr, borrowed.len) };
        Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
    }

    /// Read one `GHOSTTY_TERMINAL_DATA_*` value.
    ///
    /// # Safety
    /// `T` must be exactly the output type the C header documents for `data`.
    unsafe fn get<T: Default>(&self, data: ffi::GhosttyTerminalData) -> Result<T> {
        let mut out = T::default();
        // SAFETY: the handle is live; the caller guarantees `T` matches the documented
        // output type, so the write stays within `out`.
        let code = unsafe {
            ffi::ghostty_terminal_get(
                self.as_raw(),
                data,
                ptr::from_mut(&mut out).cast::<c_void>(),
            )
        };
        check("terminal_get", code)?;
        Ok(out)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: the handle is owned, live, and never used after this call.
        unsafe { ffi::ghostty_terminal_free(self.as_raw()) };
    }
}

impl Default for ffi::GhosttyString {
    fn default() -> Self {
        Self {
            ptr: ptr::null(),
            len: 0,
        }
    }
}

fn validate_size(cols: u16, rows: u16) -> Result<()> {
    if cols == 0 || rows == 0 {
        return Err(Error::InvalidSize { cols, rows });
    }
    Ok(())
}

/// Formatter bound to a terminal borrow; freed on drop.
struct Formatter<'t> {
    raw: NonNull<ffi::GhosttyFormatterImpl>,
    _terminal: PhantomData<&'t Terminal>,
}

impl<'t> Formatter<'t> {
    fn new(terminal: &'t Terminal, options: ffi::GhosttyFormatterTerminalOptions) -> Result<Self> {
        let mut raw: ffi::GhosttyFormatter = ptr::null_mut();
        // SAFETY: the terminal is live for `'t`; `options` (and the selection it may
        // point to) is read during this call only; `raw` is a valid out-pointer.
        let code = unsafe {
            ffi::ghostty_formatter_terminal_new(
                ptr::null(),
                &raw mut raw,
                terminal.as_raw(),
                options,
            )
        };
        check("formatter_terminal_new", code)?;
        let raw = NonNull::new(raw).ok_or(Error::NullHandle("formatter_terminal_new"))?;
        Ok(Self {
            raw,
            _terminal: PhantomData,
        })
    }

    fn format(&self) -> Result<Vec<u8>> {
        let mut buffer = OwnedBuffer::empty();
        // SAFETY: the formatter and its terminal are live; the out-pointers are valid
        // and receive a default-allocator buffer that `OwnedBuffer` frees.
        let code = unsafe {
            ffi::ghostty_formatter_format_alloc(
                self.raw.as_ptr(),
                ptr::null(),
                &raw mut buffer.ptr,
                &raw mut buffer.len,
            )
        };
        check("formatter_format_alloc", code)?;
        Ok(buffer.to_vec())
    }
}

impl Drop for Formatter<'_> {
    fn drop(&mut self) {
        // SAFETY: the formatter is owned, live, and never used after this call.
        unsafe { ffi::ghostty_formatter_free(self.raw.as_ptr()) };
    }
}

/// Buffer allocated by libghostty-vt's default allocator; freed with `ghostty_free`.
pub(crate) struct OwnedBuffer {
    pub(crate) ptr: *mut u8,
    pub(crate) len: usize,
}

impl OwnedBuffer {
    pub(crate) fn empty() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
        }
    }

    pub(crate) fn to_vec(&self) -> Vec<u8> {
        if self.ptr.is_null() {
            return Vec::new();
        }
        // SAFETY: a non-null pointer was filled in by libghostty together with `len`,
        // and stays valid until `drop`.
        unsafe { slice::from_raw_parts(self.ptr, self.len) }.to_vec()
    }
}

impl Drop for OwnedBuffer {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` came from a default-allocator libghostty allocation (or
        // are null, which `ghostty_free` ignores) and are not used afterwards.
        unsafe { ffi::ghostty_free(ptr::null(), self.ptr, self.len) };
    }
}
