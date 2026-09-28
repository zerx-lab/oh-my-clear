//! The app's asset source: gpui-kit's default component icon bundle plus the Lucide icons
//! oh-my-clear adds (sidebar categories, status facts, file kinds). Register it with
//! `application().with_assets(omc_ui::assets::Assets)`.

use std::borrow::Cow;

use gpui_kit::{AssetSource, SharedString};

gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        Broom,
        Cookie,
        CodeXml,
        Trash,
        FileClock,
        PackageX,
        FolderX,
        Disc3,
        Rocket,
        Lock,
        ShieldCheck,
        Clock,
        Sparkles,
        Film,
        Music,
        Image,
        FileArchive,
        Package,
        Power,
        AppWindow
    ]
);

/// gpui-kit's default icons and the app's own.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        match AppIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = AppIcons.list(path)?;
        paths.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;

    use super::Assets;
    use crate::nav::NAV;

    #[test]
    fn every_sidebar_icon_is_bundled() {
        let missing: Vec<_> = NAV
            .iter()
            .flat_map(|group| group.items.iter())
            .map(|category| category.icon().path())
            .filter(|path| !matches!(Assets.load(path), Ok(Some(_))))
            .collect();
        assert!(missing.is_empty(), "icons without bytes: {missing:?}");
    }
}
