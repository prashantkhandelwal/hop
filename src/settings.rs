use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub default_volume: f64,
    pub controls_hide_delay: f64,
    pub disabled_file_types: BTreeSet<FileType>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_volume: 70.0,
            controls_hide_delay: 2.0,
            disabled_file_types: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Png,
    Jpeg,
    Bmp,
    Ico,
    Tiff,
    Webp,
    Pnm,
    Tga,
    Qoi,
    Gif,
    Pdf,
    Stl,
    ThreeMf,
    Mp4,
    Mkv,
    Webm,
    Mov,
    Avi,
    Wmv,
    Mpeg,
    M4v,
    Ts,
    Mts,
    M2ts,
    Ogv,
    Flv,
    ThreeGp,
    Text,
    Folder,
}

impl FileType {
    pub const FORMATS: &[(Self, &str, &[&str])] = &[
        (Self::Png, "PNG", &["png"]),
        (Self::Jpeg, "JPEG", &["jpg", "jpeg"]),
        (Self::Bmp, "BMP", &["bmp"]),
        (Self::Ico, "ICO", &["ico"]),
        (Self::Tiff, "TIFF", &["tif", "tiff"]),
        (Self::Webp, "WebP", &["webp"]),
        (Self::Pnm, "PNM", &["pnm", "pbm", "pgm", "ppm", "pam"]),
        (Self::Tga, "TGA", &["tga"]),
        (Self::Qoi, "QOI", &["qoi"]),
        (Self::Gif, "Animated GIF", &["gif"]),
        (Self::Pdf, "PDF", &["pdf"]),
        (Self::Stl, "STL", &["stl"]),
        (Self::ThreeMf, "3MF", &["3mf"]),
        (Self::Mp4, "MP4", &["mp4"]),
        (Self::Mkv, "MKV", &["mkv"]),
        (Self::Webm, "WebM", &["webm"]),
        (Self::Mov, "MOV", &["mov"]),
        (Self::Avi, "AVI", &["avi"]),
        (Self::Wmv, "WMV", &["wmv"]),
        (Self::Mpeg, "MPEG", &["mpg", "mpeg"]),
        (Self::M4v, "M4V", &["m4v"]),
        (Self::Ts, "TS", &["ts"]),
        (Self::Mts, "MTS", &["mts"]),
        (Self::M2ts, "M2TS", &["m2ts"]),
        (Self::Ogv, "OGV", &["ogv"]),
        (Self::Flv, "FLV", &["flv"]),
        (Self::ThreeGp, "3GP", &["3gp"]),
        (Self::Text, "Text/source (any UTF-8 text file)", &[]),
        (Self::Folder, "Folders", &[]),
    ];

    pub fn for_path(path: &Path, is_directory: bool) -> Self {
        if is_directory {
            return Self::Folder;
        }
        let extension = path.extension().and_then(|extension| extension.to_str());
        Self::FORMATS
            .iter()
            .find(|(_, _, extensions)| {
                extension.is_some_and(|extension| {
                    extensions
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
            })
            .map_or(Self::Text, |(file_type, _, _)| *file_type)
    }

    pub fn is_image(self) -> bool {
        matches!(
            self,
            Self::Png
                | Self::Jpeg
                | Self::Bmp
                | Self::Ico
                | Self::Tiff
                | Self::Webp
                | Self::Pnm
                | Self::Tga
                | Self::Qoi
                | Self::Gif
        )
    }

    pub fn is_video(self) -> bool {
        matches!(
            self,
            Self::Mp4
                | Self::Mkv
                | Self::Webm
                | Self::Mov
                | Self::Avi
                | Self::Wmv
                | Self::Mpeg
                | Self::M4v
                | Self::Ts
                | Self::Mts
                | Self::M2ts
                | Self::Ogv
                | Self::Flv
                | Self::ThreeGp
        )
    }
}

impl Settings {
    pub fn path() -> Result<PathBuf, String> {
        #[cfg(windows)]
        let base = std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or("APPDATA is not set; cannot locate Hop settings.")?;
        #[cfg(not(windows))]
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            Some(value) => PathBuf::from(value),
            None => std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
                .ok_or("HOME is not set; cannot locate Hop settings.")?,
        };
        if !base.is_absolute() {
            return Err("The settings directory must be an absolute path.".into());
        }
        Ok(base.join("hop").join("settings.json"))
    }

    pub fn load() -> Result<Self, String> {
        Self::load_from(&Self::path()?)
    }

    fn load_from(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("Cannot read settings {}: {error}", path.display())),
        };
        let settings: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid settings {}: {error}", path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.default_volume.is_finite() || !(0.0..=100.0).contains(&self.default_volume) {
            return Err("Default video volume must be between 0% and 100%.".into());
        }
        if !self.controls_hide_delay.is_finite()
            || !(0.5..=30.0).contains(&self.controls_hide_delay)
        {
            return Err("Controls auto-hide delay must be between 0.5 and 30 seconds.".into());
        }
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path()?)
    }

    fn save_to(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        save_json(self, path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewPosition {
    pub x: f32,
    pub y: f32,
}

impl PreviewPosition {
    fn path() -> Result<PathBuf, String> {
        Ok(Settings::path()?.with_file_name("preview-position.json"))
    }

    pub fn load() -> Result<Option<Self>, String> {
        Self::load_from(&Self::path()?)
    }

    fn load_from(path: &Path) -> Result<Option<Self>, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Cannot read preview position {}: {error}",
                    path.display()
                ));
            }
        };
        let position: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid preview position {}: {error}", path.display()))?;
        position.validate()?;
        Ok(Some(position))
    }

    fn validate(&self) -> Result<(), String> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return Err("Preview window position must have finite coordinates.".into());
        }
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path()?)
    }

    fn save_to(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        save_json(self, path)
    }
}

fn save_json(value: &impl Serialize, path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("Settings path has no parent directory.")?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Cannot create settings directory {}: {error}",
            parent.display()
        )
    })?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("Cannot encode settings: {error}"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("Cannot create temporary settings file: {error}"))?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("Cannot write settings: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("Cannot save settings {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_position_is_optional_and_survives_reopening_and_settings_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview-position.json");
        assert_eq!(PreviewPosition::load_from(&path).unwrap(), None);
        for position in [
            PreviewPosition { x: 250.0, y: 120.0 },
            PreviewPosition {
                x: -1400.5,
                y: -80.0,
            },
        ] {
            position.save_to(&path).unwrap();
            Settings::default()
                .save_to(&directory.path().join("settings.json"))
                .unwrap();
            assert_eq!(PreviewPosition::load_from(&path).unwrap(), Some(position));
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn invalid_preview_positions_are_reported_and_do_not_overwrite_saved_position() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview-position.json");
        let position = PreviewPosition { x: 300.0, y: 200.0 };
        position.save_to(&path).unwrap();
        for invalid in [
            PreviewPosition {
                x: f32::NAN,
                y: 0.0,
            },
            PreviewPosition {
                x: 0.0,
                y: f32::INFINITY,
            },
        ] {
            assert!(invalid.save_to(&path).is_err());
            assert_eq!(PreviewPosition::load_from(&path).unwrap(), Some(position));
        }
        for bytes in [
            r#"{"x":1}"#,
            r#"{"x":null,"y":0}"#,
            r#"{"x":1e100,"y":0}"#,
            "{",
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(PreviewPosition::load_from(&path).is_err());
        }
    }

    #[test]
    fn missing_settings_use_existing_player_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::load_from(&directory.path().join("missing.json")).unwrap();
        assert_eq!(settings.default_volume, 70.0);
        assert_eq!(settings.controls_hide_delay, 2.0);
    }

    #[test]
    fn file_type_preferences_persist_and_old_settings_enable_all_formats() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{"default_volume":25,"controls_hide_delay":5}"#).unwrap();
        let mut settings = Settings::load_from(&path).unwrap();
        assert!(settings.disabled_file_types.is_empty());
        settings
            .disabled_file_types
            .extend([FileType::Jpeg, FileType::ThreeMf, FileType::Text]);
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), settings);
        Settings::default().save_to(&path).unwrap();
        assert!(
            Settings::load_from(&path)
                .unwrap()
                .disabled_file_types
                .is_empty()
        );
        for bytes in [
            r#"{"disabled_file_types":["unknown"]}"#,
            r#"{"disabled_file_types":[false]}"#,
            r#"{"disabled_file_types":null}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(Settings::load_from(&path).is_err());
        }
    }

    #[test]
    fn saves_and_replaces_settings_without_leaving_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hop").join("settings.json");
        let mut settings = Settings {
            default_volume: 25.0,
            controls_hide_delay: 5.0,
            ..Settings::default()
        };
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), settings);
        settings.default_volume = 0.0;
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), settings);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn invalid_settings_do_not_silently_reset_to_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        for bytes in [
            r#"{"default_volume":101}"#,
            r#"{"controls_hide_delay":0}"#,
            r#"{"unknown":true}"#,
            r#"{"default_volume":"loud"}"#,
            "{",
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(Settings::load_from(&path).is_err(), "{bytes}");
        }
        std::fs::write(&path, r#"{"default_volume":15}"#).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap().controls_hide_delay, 2.0);
    }

    #[test]
    fn saving_invalid_values_preserves_the_previous_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        Settings::default().save_to(&path).unwrap();
        for value in [f64::NAN, f64::INFINITY, -1.0, 101.0] {
            let invalid = Settings {
                default_volume: value,
                ..Settings::default()
            };
            assert!(invalid.save_to(&path).is_err());
            assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
        }
    }

    #[test]
    fn auto_hide_delay_enforces_the_documented_finite_range() {
        for delay in [0.5, 2.0, 30.0] {
            assert!(
                Settings {
                    controls_hide_delay: delay,
                    ..Settings::default()
                }
                .validate()
                .is_ok()
            );
        }
        for delay in [f64::NAN, f64::INFINITY, 0.0, 30.5] {
            assert!(
                Settings {
                    controls_hide_delay: delay,
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
