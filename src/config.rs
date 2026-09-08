use crate::error::ConfigError;
use std::{path::PathBuf, time::Duration};

const APP: &str = "actual-mcp";
const HOME: &str = "HOME";
const ACTUAL_SERVER_URL: &str = "ACTUAL_SERVER_URL";
const ACTUAL_PASSWORD: &str = "ACTUAL_PASSWORD";
pub(crate) const ACTUAL_SYNC_ID: &str = "ACTUAL_SYNC_ID";
const ACTUAL_CACHE_DIR: &str = "ACTUAL_CACHE_DIR";
const XDG_CACHE_HOME: &str = "XDG_CACHE_HOME";
const ACTUAL_TTL_SECONDS: &str = "ACTUAL_TTL_SECONDS";
const DEFAULT_TTL_SECONDS: u64 = 60;

#[derive(Debug)]
pub struct Config {
    pub server_url: String,
    pub password: Secret,
    pub sync_id: Option<String>,
    pub cache_dir: PathBuf,
    pub ttl: Duration,
}

impl Config {
    /// Imperative shell: reads the process environment
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// Functional core
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let server_url = required(&get, ACTUAL_SERVER_URL)?;
        let server_url = server_url.trim().trim_end_matches('/').to_string();

        // immediately wrap the password so the raw String has short life
        let password = required(&get, ACTUAL_PASSWORD).map(Secret::new)?;

        let sync_id = get(ACTUAL_SYNC_ID).filter(|v| !v.trim().is_empty());

        let cache_dir = get(ACTUAL_CACHE_DIR)
            .map(PathBuf::from)
            .or_else(|| get(XDG_CACHE_HOME).map(|p| PathBuf::from(p).join(APP)))
            .or_else(|| get(HOME).map(|h| PathBuf::from(h).join(".cache").join(APP)))
            .ok_or(ConfigError::NoCacheDir)?;

        let ttl = match get(ACTUAL_TTL_SECONDS) {
            None => Duration::from_secs(DEFAULT_TTL_SECONDS),
            Some(raw) => raw
                .trim()
                .parse::<u64>()
                .map(Duration::from_secs)
                .map_err(|e| ConfigError::Invalid {
                    var: ACTUAL_TTL_SECONDS,
                    reason: e.to_string(),
                })?,
        };

        Ok(Self {
            server_url,
            password,
            sync_id,
            cache_dir,
            ttl,
        })
    }
}

/// Helper function for reading required variables and throwing an error
/// if they are missing.
fn required(
    get: &impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<String, ConfigError> {
    get(key)
        .filter(|v| !v.trim().is_empty())
        .ok_or(ConfigError::Missing(key))
}

#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn blank_password_is_missing() {
        let err = Config::from_vars(vars(&[
            (ACTUAL_SERVER_URL, "http://localhost:11111"),
            (ACTUAL_PASSWORD, "     "),
            (HOME, "/some/path/home"),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Missing(ACTUAL_PASSWORD)));
    }

    #[test]
    fn trailing_slash_is_stripped() {
        let conf = Config::from_vars(vars(&[
            (ACTUAL_SERVER_URL, "http://localhost:11111/////"),
            (ACTUAL_PASSWORD, "insecurepwd123"),
            (HOME, "/some/path/home"),
        ]))
        .unwrap();
        assert_eq!("http://localhost:11111", conf.server_url);
    }

    #[test]
    fn cache_dir_prefers_explicit_over_xdg() {
        let conf = Config::from_vars(vars(&[
            (ACTUAL_SERVER_URL, "http://localhost:11111/////"),
            (ACTUAL_PASSWORD, "insecurepwd123"),
            (ACTUAL_CACHE_DIR, "/cache"),
            (XDG_CACHE_HOME, "/xdgcache"),
            (HOME, "/some/path/home"),
        ]))
        .unwrap();
        assert_eq!(PathBuf::from("/cache"), conf.cache_dir);
    }
}
