//! The oh-my-clear logo inside the app. Sources are `assets/brand/*.svg`; the rasters in
//! `crates/omc-ui/assets/brand/` are rendered by `assets/brand/render.sh`.
//!
//! Platform app icons live outside this crate: the macOS bundle icon
//! (`apps/oh-my-clear/resources/macos`, assembled by `cargo omc`), the Windows executable
//! icon (`apps/oh-my-clear/build.rs`) and the Linux desktop entry + hicolor icons
//! (`apps/oh-my-clear/resources/linux`, matched through the window's `app_id`).

use std::sync::{Arc, LazyLock};

use gpui_kit::{Image, ImageFormat};

/// `mark.svg` at 64 px: sharp on 2× displays at the overview page's 32 px. An SVG `img`
/// would be rasterised at its intrinsic 1024 px.
static MARK: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../assets/brand/mark.png").to_vec(),
    ))
});

/// The logo mark, for an `img` element.
pub(crate) fn mark() -> Arc<Image> {
    Arc::clone(&MARK)
}

/// X11 `_NET_WM_ICON` for every window, the 128 px app icon; `None` (logged) if it fails
/// to decode. Wayland ignores it and resolves the icon from the desktop entry named by
/// the window's `app_id`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) fn window_icon() -> Option<Arc<image::RgbaImage>> {
    static ICON: LazyLock<Option<Arc<image::RgbaImage>>> = LazyLock::new(|| {
        match image::load_from_memory_with_format(
            include_bytes!("../assets/brand/window-icon.png"),
            image::ImageFormat::Png,
        ) {
            Ok(icon) => Some(Arc::new(icon.into_rgba8())),
            Err(err) => {
                tracing::error!("cannot decode the window icon: {err}");
                None
            }
        }
    });
    ICON.clone()
}
