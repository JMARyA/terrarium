use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    url: Option<String>,
    username: Option<String>,
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
///   3. Interactive password prompt if TERRARIUM_PASSWORD is unset
///
/// Passwords are intentionally excluded from config files.
pub fn load(explicit_config_path: Option<PathBuf>) -> Result<ClientConfig, String> {
    let file = load_config_file(explicit_config_path).unwrap_or_default();

    let url = std::env::var("TERRARIUM_URL")
        .ok()
        .or(file.url)
        .ok_or("Server URL not set. Use TERRARIUM_URL or add 'url' to config file.")?;

    let username = std::env::var("TERRARIUM_USER")
        .ok()
        .or(file.username)
        .ok_or("Username not set. Use TERRARIUM_USER or add 'username' to config file.")?;

    let password = std::env::var("TERRARIUM_PASSWORD")
        .ok()
        .unwrap_or_else(|| crate::readline("Password: "));

    Ok(ClientConfig {
        url: url.trim_end_matches('/').to_string(),
        username,
        password,
    })
}

fn load_config_file(explicit_path: Option<PathBuf>) -> Option<ConfigFile> {
    let path = explicit_path
        .or_else(|| std::env::var("TERRARIUM_CONFIG").ok().map(PathBuf::from))
        .or_else(|| dirs::config_dir().map(|d| d.join("terrarium").join("config.toml")))
        .or_else(|| dirs::home_dir().map(|d| d.join(".terrarium.toml")))?;

    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}
