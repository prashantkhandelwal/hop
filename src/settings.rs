use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub default_volume: f64,
    pub controls_hide_delay: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_volume: 70.0,
            controls_hide_delay: 2.0,
        }
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
        let parent = path
            .parent()
            .ok_or("Settings path has no parent directory.")?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Cannot create settings directory {}: {error}",
                parent.display()
            )
        })?;
        let bytes = serde_json::to_vec_pretty(self)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_use_existing_player_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::load_from(&directory.path().join("missing.json")).unwrap();
        assert_eq!(settings.default_volume, 70.0);
        assert_eq!(settings.controls_hide_delay, 2.0);
    }

    #[test]
    fn saves_and_replaces_settings_without_leaving_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hop").join("settings.json");
        let mut settings = Settings {
            default_volume: 25.0,
            controls_hide_delay: 5.0,
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
