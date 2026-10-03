//! URL helpers.

use url::Url;

use crate::error::{Error, Result};

/// Normalizes a Coroot URL: adds `http://` when the scheme is missing, keeps a base path
/// (`--url-base-path`), strips UI routes copied from the browser (`/p/<project>/...`),
/// query and fragment, and ends with a slash.
pub fn normalize_base_url(url: &str) -> Result<Url> {
    let url = url.trim();
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    };
    let mut u = Url::parse(&with_scheme)
        .map_err(|e| Error::invalid(format!("invalid URL '{url}': {e}")))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(Error::invalid(format!(
            "unsupported URL scheme '{}'",
            u.scheme()
        )));
    }
    let path = u.path().to_string();
    let mut path = match path.find("/p/") {
        Some(i) => path[..i + 1].to_string(),
        None => path,
    };
    if !path.ends_with('/') {
        path.push('/');
    }
    u.set_path(&path);
    u.set_query(None);
    u.set_fragment(None);
    Ok(u)
}

/// Percent-encodes a value for use as one URL path segment (like JavaScript's
/// `encodeURIComponent`). Application ids contain `:`, which Coroot expects encoded.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Coroot serves its single-page UI, with a 200, for paths it does not know.
pub fn is_html(body: &str) -> bool {
    let start = body
        .trim_start()
        .get(..15)
        .unwrap_or_default()
        .to_ascii_lowercase();
    start.starts_with("<!doctype html") || start.starts_with("<html")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls() {
        assert_eq!(
            normalize_base_url("localhost:8080").unwrap().as_str(),
            "http://localhost:8080/"
        );
        assert_eq!(
            normalize_base_url("https://c.example.com/coroot")
                .unwrap()
                .as_str(),
            "https://c.example.com/coroot/"
        );
        assert_eq!(
            normalize_base_url("https://c.example.com/coroot/p/abc/applications?from=x")
                .unwrap()
                .as_str(),
            "https://c.example.com/coroot/"
        );
        assert!(normalize_base_url("ftp://x").is_err());
    }

    #[test]
    fn segments() {
        assert_eq!(
            encode_segment("c1:default:Deployment:api"),
            "c1%3Adefault%3ADeployment%3Aapi"
        );
        assert_eq!(encode_segment("a b/c"), "a%20b%2Fc");
    }
}
