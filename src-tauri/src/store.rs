//! The app's data folder: small JSON documents plus recorded activity files.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

pub struct Store {
    dir: PathBuf,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 80 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, name: &str) -> Result<PathBuf, String> {
        if !valid_name(name) {
            return Err(format!("Invalid store key: {name}"));
        }
        Ok(self.dir.join("data").join(format!("{name}.json")))
    }

    pub fn get<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let text = fs::read_to_string(self.file(name).ok()?).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn set<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        let file = self.file(name)?;
        fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        // Write-then-rename so a crash mid-write can't leave a truncated file behind.
        let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, serde_json::to_vec(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &file).map_err(|e| e.to_string())
    }

    pub fn del(&self, name: &str) -> Result<(), String> {
        match fs::remove_file(self.file(name)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn activities_dir(&self) -> PathBuf {
        self.dir.join("activities")
    }

    /// True if `path` is a recording this app wrote — the only files it will upload or reveal.
    pub fn owns_activity(&self, path: &Path) -> bool {
        match (path.canonicalize(), self.activities_dir().canonicalize()) {
            (Ok(p), Ok(dir)) => p.starts_with(dir),
            _ => false,
        }
    }

    pub fn save_activity(&self, file_name: &str, bytes: &[u8], export_dir: &str) -> Result<PathBuf, String> {
        let dir = self.activities_dir();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(file_name);
        fs::write(&path, bytes).map_err(|e| format!("Could not write the ride file: {e}"))?;
        if !export_dir.is_empty() {
            let copied = fs::create_dir_all(export_dir).and_then(|()| fs::copy(&path, Path::new(export_dir).join(file_name)));
            if let Err(e) = copied {
                eprintln!("Could not copy the activity to the export folder: {e}");
            }
        }
        Ok(path)
    }
}
