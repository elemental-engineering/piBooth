//! Fonts and status icons loaded from the assets directory (v1's
//! `fonts/` and `images/` folders).

use std::path::Path;

use anyhow::Context;
use fontdue::Font;
use image::imageops::FilterType;

use super::Rgba;
use super::text::load_font;

pub const TITLE_FONT: &str = "fonts/Carnevalee Freakshow.ttf";
pub const TEXT_FONT: &str = "fonts/arial.ttf";

/// Status icons in v1's left-to-right order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    HddWarning,
    HddCritical,
    NoNetwork,
    TempWarning,
    TempCritical,
}

impl Icon {
    pub const ALL: [Icon; 5] =
        [Icon::HddWarning, Icon::HddCritical, Icon::NoNetwork, Icon::TempWarning, Icon::TempCritical];

    pub fn file(self) -> &'static str {
        match self {
            Icon::HddWarning => "images/hdd_warning.png",
            Icon::HddCritical => "images/hdd_critical.png",
            Icon::NoNetwork => "images/network.png",
            Icon::TempWarning => "images/temp_warning.png",
            Icon::TempCritical => "images/temp_critical.png",
        }
    }
}

pub struct Assets {
    pub title_font: Font,
    pub text_font: Font,
    /// Indexed like [`Icon::ALL`], each `icon_px` square.
    pub icons: Vec<Rgba>,
}

impl Assets {
    pub fn load(dir: &Path, icon_px: u32) -> anyhow::Result<Self> {
        let font = |rel: &str| -> anyhow::Result<Font> {
            let p = dir.join(rel);
            let bytes = std::fs::read(&p).with_context(|| format!("reading font {}", p.display()))?;
            load_font(bytes).with_context(|| format!("loading font {}", p.display()))
        };
        let icons = Icon::ALL
            .iter()
            .map(|icon| load_icon(&dir.join(icon.file()), icon_px))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { title_font: font(TITLE_FONT)?, text_font: font(TEXT_FONT)?, icons })
    }

    pub fn icon(&self, icon: Icon) -> &Rgba {
        &self.icons[Icon::ALL.iter().position(|i| *i == icon).unwrap()]
    }
}

/// Decode a PNG and scale it to `px` × `px`.
pub fn load_icon(path: &Path, px: u32) -> anyhow::Result<Rgba> {
    let img = image::open(path).with_context(|| format!("loading icon {}", path.display()))?.to_rgba8();
    let img = image::imageops::resize(&img, px.max(1), px.max(1), FilterType::Lanczos3);
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_assets() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
    }

    #[test]
    fn loads_v1_assets_at_requested_icon_size() {
        let a = Assets::load(repo_assets(), 32).unwrap();
        assert_eq!(a.icons.len(), 5);
        for icon in &a.icons {
            assert_eq!((icon.width, icon.height, icon.data.len()), (32, 32, 32 * 32 * 4));
        }
        assert!(a.icon(Icon::NoNetwork).data.chunks(4).any(|p| p[3] > 0), "icon has visible pixels");
        assert!(a.title_font.horizontal_line_metrics(100.0).is_some());
    }

    #[test]
    fn missing_assets_name_the_path() {
        let err = Assets::load(Path::new("/nonexistent"), 32).err().unwrap();
        assert!(format!("{err:#}").contains("/nonexistent/"), "{err:#}");
    }
}
