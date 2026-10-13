//! Where the Strava credentials live: the OS keychain on macOS and Windows; on
//! Linux, where no keyring service is guaranteed to exist, a file readable only
//! by the user.

use std::path::Path;

const SERVICE: &str = "WinterMiles";
const ACCOUNT: &str = "strava";

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn load(_dir: &Path) -> Option<String> {
    keyring::Entry::new(SERVICE, ACCOUNT).ok()?.get_password().ok()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn save(_dir: &Path, secret: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, ACCOUNT).and_then(|e| e.set_password(secret)).map_err(|e| format!("Could not save to the system keychain: {e}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn load(dir: &Path) -> Option<String> {
    let _ = (SERVICE, ACCOUNT);
    std::fs::read_to_string(dir.join("strava.json")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn save(dir: &Path, secret: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir.join("strava.json")).map_err(|e| e.to_string())?;
    file.write_all(secret.as_bytes()).map_err(|e| e.to_string())
}
