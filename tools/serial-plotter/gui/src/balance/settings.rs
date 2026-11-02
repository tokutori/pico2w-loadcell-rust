//! Versioned UTF-8 settings, isolated from calculation and UI.
use crate::balance::Config;
use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum Error {
    Read(std::io::Error),
    Json(serde_json::Error),
    Validation(super::Error),
    Write(std::io::Error),
    Clock(std::time::SystemTimeError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "Cannot read settings: {e}"),
            Self::Json(e) => write!(f, "Invalid settings JSON: {e}"),
            Self::Validation(e) => write!(f, "{e}"),
            Self::Write(e) => write!(f, "Cannot save settings: {e}"),
            Self::Clock(e) => write!(f, "Cannot generate settings temporary filename: {e}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(e) | Self::Write(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Validation(e) => Some(e),
            Self::Clock(e) => Some(e),
        }
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("LoadCellSerialPlotter").join("balance.json")
}

pub fn load(path: &Path) -> Result<Config, Error> {
    let text = fs::read_to_string(path).map_err(Error::Read)?;
    let config: Config = serde_json::from_str(&text).map_err(Error::Json)?;
    config.validate().map_err(Error::Validation)?;
    Ok(config)
}

pub fn save(path: &Path, config: &Config) -> Result<(), Error> {
    config.validate().map_err(Error::Validation)?;
    let json = serde_json::to_vec_pretty(config).map_err(Error::Json)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(Error::Write)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(Error::Clock)?
        .as_nanos();
    let temp = parent.join(format!(".balance-{}-{stamp}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&json)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(Error::Write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_replace_and_invalid_save_preserves_previous_file() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("loadcell-settings-{}-{stamp}", std::process::id()));
        let path = dir.join("測定設定.json");
        let mut config = Config::default();
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
        config.cells[1].zero_raw = Some(-123.5);
        config.layout.points[0][0] = 400.0;
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
        let before = fs::read(&path).unwrap();
        config.cells[0].counts_per_kg = f64::NAN;
        assert!(save(&path, &config).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::write(&path, "{ broken JSON }").unwrap();
        assert!(load(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
}
