use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize, Default)]
struct ConfigFile {
    url: Option<String>,
    username: Option<String>,
    /// Password stored in plaintext — file must be chmod 600.
    password: Option<String>,
}

#[derive(Debug)]
pub struct ClientConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

/// Load client config from (in priority order):
///   1. TERRARIUM_URL / TERRARIUM_USER / TERRARIUM_PASSWORD env vars
///   2. Config file (explicit path > TERRARIUM_CONFIG > ~/.config/terrarium/config.toml > ~/.terrarium.toml)
///   3. Interactive password prompt if all of the above are unset
///
/// Config files containing passwords must be chmod 600; a too-open file is
/// rejected (same policy as SSH identity files).
pub fn load(explicit_config_path: Option<PathBuf>) -> Result<ClientConfig, String> {
    let file = load_config_file(explicit_config_path)?.unwrap_or_default();

    let url = std::env::var("TERRARIUM_URL").ok().or(file.url).ok_or(
        "Server URL not set. Use TERRARIUM_URL, add 'url' to config file, or run 'terra login'.",
    )?;

    let username = std::env::var("TERRARIUM_USER")
        .ok()
        .or(file.username)
        .ok_or("Username not set. Use TERRARIUM_USER, add 'username' to config file, or run 'terra login'.")?;

    let password = std::env::var("TERRARIUM_PASSWORD")
        .ok()
        .or(file.password)
        .unwrap_or_else(|| crate::readline("Password: "));

    Ok(ClientConfig {
        url: url.trim_end_matches('/').to_string(),
        username,
        password,
    })
}

/// Write a config file with url, username, and password, then chmod 600.
/// Creates parent directories as needed.
pub fn write(path: &PathBuf, url: &str, username: &str, password: &str) -> Result<(), String> {
    let cfg = ConfigFile {
        url: Some(url.trim_end_matches('/').to_string()),
        username: Some(username.to_string()),
        password: Some(password.to_string()),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {e}"))?;
    }

    let content = toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, &content).map_err(|e| format!("Failed to write config: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to set config file permissions: {e}"))?;
    }

    Ok(())
}

/// Returns Ok(None) when no config file exists, Ok(Some(...)) on success,
/// Err(...) on permission violations or parse failures.
fn load_config_file(explicit_path: Option<PathBuf>) -> Result<Option<ConfigFile>, String> {
    let path = match explicit_path
        .or_else(|| std::env::var("TERRARIUM_CONFIG").ok().map(PathBuf::from))
        .or_else(|| dirs::config_dir().map(|d| d.join("terrarium").join("config.toml")))
        .or_else(|| dirs::home_dir().map(|d| d.join(".terrarium.toml")))
    {
        Some(p) => p,
        None => return Ok(None),
    };

    if !path.exists() {
        return Ok(None);
    }

    #[cfg(unix)]
    check_permissions(&path)?;

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config file {:?}: {e}", path))?;
    let cfg = toml::from_str(&content)
        .map_err(|e| format!("Failed to parse config file {:?}: {e}", path))?;
    Ok(Some(cfg))
}

/// Reject config files with group- or world-readable/writable bits set.
/// Returns Err with a human-readable message if permissions are too open.
#[cfg(unix)]
fn check_permissions(path: &PathBuf) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "Config file {:?} has insecure permissions ({:04o}).\n\
             Fix with: chmod 600 {:?}",
            path,
            mode & 0o777,
            path
        ));
    }
    Ok(())
}
