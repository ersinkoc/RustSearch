use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == "rustsearch.svg" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/rustsearch.svg"
            ))));
        }
        gpui_component_assets::Assets.load(path)
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut assets = gpui_component_assets::Assets.list(path)?;
        if "rustsearch.svg".starts_with(path) {
            assets.push("rustsearch.svg".into());
        }
        Ok(assets)
    }
}
