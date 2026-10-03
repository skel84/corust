//! Persistent configuration: named contexts, each pointing at a Coroot instance.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_context: Option<String>,
    #[serde(default)]
    pub contexts: BTreeMap<String, Context>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Base URL of the Coroot UI, including any --url-base-path.
    pub url: String,
    /// User API key ("crt_...") sent as a Bearer token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Session cookie obtained by logging in with email and password.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Default project id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Skip TLS certificate verification.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
}

pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CORUST_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()
            .context("cannot determine home directory")?
            .join(".config"),
    };
    Ok(base.join("corust").join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = path()?;
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s)
                .with_context(|| format!("invalid config file {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let data = toml::to_string_pretty(self)?;
        write_private(&path, data.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Returns the named context, or the current one when `name` is None.
    pub fn context(&self, name: Option<&str>) -> Result<Option<(&str, &Context)>> {
        let name = match name.or(self.current_context.as_deref()) {
            Some(n) => n,
            None => return Ok(None),
        };
        match self.contexts.get_key_value(name) {
            Some((k, v)) => Ok(Some((k.as_str(), v))),
            None => bail!("context '{name}' not found (see `corust config contexts`)"),
        }
    }
}

/// Derives a context name from a URL, e.g. "https://coroot.example.com:8080" -> "coroot.example.com".
pub fn context_name_for(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?;
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host.to_string(),
            })
        })
        .unwrap_or_else(|| "default".into())
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("toml.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(tmp, path)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut cfg = Config::default();
        cfg.contexts.insert(
            "prod".into(),
            Context {
                url: "https://c.example.com".into(),
                token: Some("crt_x".into()),
                ..Default::default()
            },
        );
        cfg.current_context = Some("prod".into());
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(s.contains("current-context = \"prod\""));
        assert!(!s.contains("insecure"));
        let back: Config = toml::from_str(&s).unwrap();
        let (name, ctx) = back.context(None).unwrap().unwrap();
        assert_eq!(name, "prod");
        assert_eq!(ctx.token.as_deref(), Some("crt_x"));
        assert!(back.context(Some("nope")).is_err());
    }

    #[test]
    fn names() {
        assert_eq!(
            context_name_for("https://coroot.example.com:8080/x"),
            "coroot.example.com:8080"
        );
        assert_eq!(context_name_for("garbage"), "default");
    }
}
