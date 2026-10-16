//! Serve library: whether this machine serves its library, on which port,
//! and the token a client must present. Kept beside rbxport's own data, in a
//! fork-named file, because the server starts before any window does.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 48650;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub serve: bool,
    pub port: u16,
    pub token: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { serve: false, port: DEFAULT_PORT, token: String::new() }
    }
}

fn path() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("rbxport").join("fork-server.json")
}

pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// The stored settings, with a token made and saved the first time.
pub fn load() -> Settings {
    let mut settings: Settings =
        std::fs::read(path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    if settings.token.is_empty() {
        settings.token = new_token();
        if let Err(e) = save(&settings) {
            tracing::warn!(error = %e, "could not save the server settings");
        }
    }
    settings
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, bytes)?;
    #[cfg(unix)]
    std::fs::set_permissions(&staged, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    std::fs::rename(staged, path)
}
