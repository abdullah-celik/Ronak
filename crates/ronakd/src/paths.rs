//! Where Ronak keeps things, following the XDG base-directory convention on
//! Linux (and the platform equivalents elsewhere, via the `dirs` crate):
//!
//! - config:  ~/.config/ronak/config.toml
//! - data:    ~/.local/share/ronak/        (the encrypted database)
//! - runtime: $XDG_RUNTIME_DIR/ronak/      (the control socket; a tmpfs that
//!   only you can read, wiped at logout)
//!
//! Each can be overridden with an environment variable, which the dev scripts
//! use to keep a nested test session apart from your real one.

use std::path::PathBuf;

use anyhow::{Context, Result};
use ronak_core::config::{Config, Encryption};

pub fn config_file() -> PathBuf {
    std::env::var_os("RONAK_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ronak").join("config.toml"))
}

pub fn data_dir(cfg: &Config) -> PathBuf {
    if let Some(dir) = std::env::var_os("RONAK_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = &cfg.storage.data_dir {
        return PathBuf::from(dir);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("ronak")
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("RONAK_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::runtime_dir().map(|d| d.join("ronak")))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("ronak-{}", whoami())))
}

pub fn control_socket() -> PathBuf {
    runtime_dir().join("control.sock")
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

/// Loads the config file if it exists, then applies environment overrides.
pub fn load_config() -> Result<Config> {
    let path = config_file();
    let mut cfg: Config = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    if std::env::var("RONAK_ENCRYPTION").is_ok_and(|v| v == "none") {
        cfg.storage.encryption = Encryption::None;
    }
    Ok(cfg)
}

/// Creates a directory readable only by the current user.
pub fn private_dir(path: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
