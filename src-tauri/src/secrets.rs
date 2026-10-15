//! Where credentials live (Strava tokens, the Home Assistant token): the OS
//! keychain on macOS and Windows; on Linux, where no keyring service is
//! guaranteed to exist, a file per secret readable only by the user.

use std::path::Path;

const SERVICE: &str = "WinterMiles";

pub fn load(dir: &Path) -> Option<String> {
    load_named(dir, "strava")
}

pub fn save(dir: &Path, secret: &str) -> Result<(), String> {
    save_named(dir, "strava", secret)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn load_named(_dir: &Path, account: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, account).ok()?.get_password().ok()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn save_named(_dir: &Path, account: &str, secret: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, account).and_then(|e| e.set_password(secret)).map_err(|e| format!("Could not save to the system keychain: {e}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn load_named(dir: &Path, account: &str) -> Option<String> {
    let _ = SERVICE;
    std::fs::read_to_string(dir.join(format!("{account}.json"))).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn save_named(dir: &Path, account: &str, secret: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir.join(format!("{account}.json"))).map_err(|e| e.to_string())?;
    file.write_all(secret.as_bytes()).map_err(|e| e.to_string())
}
