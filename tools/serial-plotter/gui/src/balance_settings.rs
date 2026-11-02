//! Versioned UTF-8 settings, isolated from calculation and UI.
use crate::balance::Config;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("LoadCellSerialPlotter").join("balance.json")
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("Cannot read settings: {e}"))?;
    let config: Config =
        serde_json::from_str(&text).map_err(|e| format!("Invalid settings JSON: {e}"))?;
    config.validate()?;
    Ok(config)
}

pub fn save(path: &Path, config: &Config) -> Result<(), String> {
    config.validate()?;
    let json = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
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
    result.map_err(|e| format!("Cannot save settings: {e}"))
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
