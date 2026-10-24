use anyhow::{Context, Result, ensure};
use gpui::WindowAppearance;
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path, sync::OnceLock};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}
impl Appearance {
    pub fn resolve(self, system: WindowAppearance) -> Mode {
        match self {
            Self::Light => Mode::Light,
            Self::Dark => Mode::Dark,
            Self::System => match system {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Mode::Dark,
                _ => Mode::Light,
            },
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::System),
            Err(error) => Err(error.into()),
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.len() <= 1024,
                    "invalid appearance preferences"
                );
                let preferences: Preferences = serde_json::from_slice(&fs::read(path)?)?;
                Ok(preferences.appearance)
            }
        }
    }
    pub fn save(self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("appearance preferences need a parent")?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".appearance-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&Preferences { appearance: self })?)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err()
            && let Err(error) = fs::remove_file(&temporary)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Unable to clean up appearance preferences");
        }
        result
    }
}
#[derive(Serialize, Deserialize)]
struct Preferences {
    appearance: Appearance,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub canvas: u32,
    pub panel: u32,
    pub sidebar: u32,
    pub control: u32,
    pub hover: u32,
    pub selected: u32,
    pub selected_text: u32,
    pub border: u32,
    pub control_border: u32,
    pub separator: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub focus: u32,
    pub positive: u32,
    pub negative: u32,
    pub warning: u32,
    pub info_surface: u32,
    pub warning_surface: u32,
    pub chart: u32,
    pub chart_empty: u32,
}
#[derive(Deserialize)]
struct Scales {
    light: Palette,
    dark: Palette,
}
#[derive(Deserialize)]
struct Palette {
    slate: [String; 12],
    blue: [String; 12],
    green: [String; 12],
    red: [String; 12],
    amber: [String; 12],
}
impl Colors {
    pub fn new(mode: Mode) -> Self {
        static SCALES: OnceLock<Scales> = OnceLock::new();
        let scales = SCALES.get_or_init(|| {
            serde_json::from_str(include_str!("../assets/color-scales.json"))
                .expect("validated bundled color scales")
        });
        let palette = match mode {
            Mode::Light => &scales.light,
            Mode::Dark => &scales.dark,
        };
        let color = |scale: &[String; 12], step: usize| {
            u32::from_str_radix(scale[step - 1].trim_start_matches('#'), 16)
                .expect("validated bundled color token")
        };
        Self {
            canvas: color(&palette.slate, 1),
            panel: if mode == Mode::Light {
                0xffffff
            } else {
                color(&palette.slate, 2)
            },
            sidebar: color(&palette.slate, 2),
            control: color(&palette.slate, 3),
            hover: color(&palette.slate, 4),
            selected: color(&palette.blue, 4),
            selected_text: color(&palette.blue, 12),
            border: color(&palette.slate, 6),
            control_border: color(&palette.slate, 7),
            separator: color(&palette.slate, 3),
            text: color(&palette.slate, 12),
            muted: color(&palette.slate, 11),
            accent: color(&palette.blue, 11),
            focus: color(&palette.blue, 9),
            positive: color(&palette.green, 11),
            negative: color(&palette.red, 11),
            warning: color(&palette.amber, 11),
            info_surface: color(&palette.blue, 2),
            warning_surface: color(&palette.amber, 3),
            chart: color(&palette.blue, 9),
            chart_empty: color(&palette.slate, 4),
        }
    }
}
/// Shared density and geometry. Values follow a four-pixel spacing grid.
#[derive(Clone, Copy)]
pub struct Metrics {
    pub space: [f32; 9],
    pub caption: f32,
    pub body: f32,
    pub title: f32,
    pub control_radius: f32,
    pub panel_radius: f32,
    pub control_height: f32,
    pub toolbar_height: f32,
    pub sidebar_width: f32,
    pub list_width: f32,
    pub icon: f32,
}
pub const METRICS: Metrics = Metrics {
    space: [4., 8., 12., 16., 24., 32., 40., 48., 64.],
    caption: 11.,
    body: 13.,
    title: 18.,
    control_radius: 6.,
    panel_radius: 8.,
    control_height: 32.,
    toolbar_height: 56.,
    sidebar_width: 216.,
    list_width: 440.,
    icon: 16.,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_changes_do_not_override_explicit_appearance() {
        assert_eq!(
            Appearance::System.resolve(WindowAppearance::Dark),
            Mode::Dark
        );
        assert_eq!(
            Appearance::System.resolve(WindowAppearance::Light),
            Mode::Light
        );
        assert_eq!(
            Appearance::System.resolve(WindowAppearance::VibrantDark),
            Mode::Dark
        );
        assert_eq!(
            Appearance::Light.resolve(WindowAppearance::Dark),
            Mode::Light
        );
        assert_eq!(
            Appearance::Dark.resolve(WindowAppearance::Light),
            Mode::Dark
        );
    }
    #[test]
    fn preferences_round_trip_privately_and_bad_files_fail_explicitly() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("appearance-{}", uuid::Uuid::new_v4()));
        let path = root.join("appearance.json");
        assert_eq!(Appearance::load(&path).unwrap(), Appearance::System);
        Appearance::Dark.save(&path).unwrap();
        assert_eq!(Appearance::load(&path).unwrap(), Appearance::Dark);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        Appearance::System.save(&path).unwrap();
        assert_eq!(Appearance::load(&path).unwrap(), Appearance::System);
        fs::write(&path, b"invalid").unwrap();
        assert!(Appearance::load(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    fn contrast(left: u32, right: u32) -> f64 {
        let luminance = |color: u32| {
            let linear = |channel: u32| {
                let channel = channel as f64 / 255.;
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            linear((color >> 16) & 255) * 0.2126
                + linear((color >> 8) & 255) * 0.7152
                + linear(color & 255) * 0.0722
        };
        let (left, right) = (luminance(left), luminance(right));
        (left.max(right) + 0.05) / (left.min(right) + 0.05)
    }
    #[test]
    fn normal_text_and_selected_labels_have_readable_contrast_in_both_modes() {
        for mode in [Mode::Light, Mode::Dark] {
            let colors = Colors::new(mode);
            for (foreground, background) in [
                (colors.text, colors.canvas),
                (colors.muted, colors.sidebar),
                (colors.text, colors.panel),
                (colors.selected_text, colors.selected),
                (colors.accent, colors.panel),
                (colors.positive, colors.panel),
                (colors.negative, colors.panel),
                (colors.warning, colors.panel),
            ] {
                assert!(
                    contrast(foreground, background) >= 4.5,
                    "contrast below 4.5: {mode:?} {foreground:x}/{background:x}"
                );
            }
        }
    }
}
