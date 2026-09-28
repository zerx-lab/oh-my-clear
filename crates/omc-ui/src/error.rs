//! omc-ui error type. GPUI reports failures as `anyhow::Error`; they are rendered to text
//! with `{:#}` at this edge, so anyhow never becomes a dependency (ADR 0004).

/// Failures while bootstrapping the UI (fonts, themes, key bindings, windows).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The platform text system rejected the bundled font files.
    #[error("failed to register bundled fonts: {0}")]
    Fonts(String),
    /// A bundled theme preset failed to parse.
    #[error("bundled theme preset `{file}` is invalid: {message}")]
    ThemePreset {
        /// File name under `crates/omc-ui/themes/`.
        file: &'static str,
        /// Parser message.
        message: String,
    },
    /// A key binding string or context predicate failed to parse.
    #[error("invalid key binding `{keys}`: {message}")]
    KeyBinding {
        /// The keystroke source, e.g. `secondary-,`.
        keys: &'static str,
        /// Parser message.
        message: String,
    },
    /// The platform refused to open a window.
    #[error("failed to open the {window} window: {message}")]
    Window {
        /// Which window, for the log line.
        window: &'static str,
        /// Platform message.
        message: String,
    },
}

/// omc-ui result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
