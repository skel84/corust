//! Typed errors with stable kinds and exit codes, so scripts and agents can react to failures.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Invalid arguments or configuration.
    Usage,
    /// Missing or rejected credentials.
    Auth,
    /// Authenticated, but the role does not allow it.
    Forbidden,
    /// The project, application, node, alert, ... does not exist.
    NotFound,
    /// A short name matched several objects; see `candidates`.
    Ambiguous,
    /// Coroot could not be reached.
    Network,
    /// Coroot returned an unexpected error.
    Server,
    /// The Coroot version does not support the requested feature.
    Unsupported,
    /// A response was larger than CORUST_MAX_RESPONSE_BYTES allows.
    ResponseTooLarge,
}

impl Kind {
    pub fn exit_code(self) -> i32 {
        match self {
            Kind::Usage => 2,
            Kind::Auth => 3,
            Kind::Forbidden => 4,
            Kind::NotFound => 5,
            Kind::Ambiguous => 6,
            Kind::Network => 7,
            Kind::Server => 8,
            Kind::Unsupported => 9,
            Kind::ResponseTooLarge => 10,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CliError {
    pub kind: Kind,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        for c in self.candidates.iter().take(20) {
            write!(f, "\n  {c}")?;
        }
        if self.candidates.len() > 20 {
            write!(f, "\n  … and {} more", self.candidates.len() - 20)?;
        }
        Ok(())
    }
}

impl std::error::Error for CliError {}

pub fn err(kind: Kind, message: impl Into<String>) -> anyhow::Error {
    CliError {
        kind,
        message: message.into(),
        candidates: Vec::new(),
    }
    .into()
}

pub fn ambiguous(message: impl Into<String>, mut candidates: Vec<String>) -> anyhow::Error {
    candidates.sort();
    CliError {
        kind: Kind::Ambiguous,
        message: message.into(),
        candidates,
    }
    .into()
}

/// Converts a library error, adding hints that point to corust commands.
fn from_lib(e: &coroot_rs::Error) -> Option<CliError> {
    use coroot_rs::ErrorKind as K;
    let kind = match e.kind() {
        K::InvalidInput => Kind::Usage,
        K::Auth => Kind::Auth,
        K::Forbidden => Kind::Forbidden,
        K::NotFound => Kind::NotFound,
        K::Ambiguous => Kind::Ambiguous,
        K::Network => Kind::Network,
        K::Unsupported => Kind::Unsupported,
        K::ResponseTooLarge => Kind::ResponseTooLarge,
        // Malformed responses stay untyped (exit code 1).
        K::Decode => return None,
        _ => Kind::Server,
    };
    let m = e.message();
    let message = match kind {
        Kind::Auth if m == "authentication required" => {
            format!("{m}: run `corust login` or set COROOT_TOKEN")
        }
        Kind::Auth if m.starts_with("unauthorized: the session has expired") => {
            format!("{m}, run `corust login` again")
        }
        Kind::Auth if m.contains("no admin password yet") => format!("{m}, then run `corust login`"),
        Kind::Auth if m.contains("MCP endpoint requires an API key") => {
            "this command uses Coroot's MCP endpoint, which requires an API key: \
             create one in the Coroot UI (user menu → API keys) and run `corust login --token <key>`"
                .to_string()
        }
        Kind::Network if m.contains(": timed out") => m.replacen(
            ": timed out",
            ": timed out (set CORUST_TIMEOUT to raise the limit, in seconds)",
            1,
        ),
        Kind::NotFound if m.starts_with("application '") && m.ends_with("' not found") => {
            format!("{m} (see `corust apps`)")
        }
        Kind::NotFound if m.starts_with("node '") && m.ends_with("' not found") => {
            format!("{m} (see `corust nodes`)")
        }
        Kind::NotFound if m.starts_with("trace ") && m.ends_with("selected time range") => {
            format!("{m} (try --since 24h)")
        }
        _ => m.to_string(),
    };
    Some(CliError {
        kind,
        message,
        candidates: e.candidates().to_vec(),
    })
}

/// The typed error in an error chain, if any.
fn typed(e: &anyhow::Error) -> Option<CliError> {
    e.chain().find_map(|c| {
        c.downcast_ref::<CliError>()
            .cloned()
            .or_else(|| c.downcast_ref::<coroot_rs::Error>().and_then(from_lib))
    })
}

/// Exit code for any error: typed errors use their kind, everything else is 1.
pub fn exit_code(e: &anyhow::Error) -> i32 {
    typed(e).map(|c| c.kind.exit_code()).unwrap_or(1)
}

/// Prints an error to stderr, as JSON for machine output formats.
pub fn report(e: &anyhow::Error, machine: bool) {
    let typed = typed(e);
    if machine {
        let body = match typed {
            Some(t) => serde_json::json!({"error": t}),
            None => serde_json::json!({"error": {"kind": "error", "message": format!("{e:#}")}}),
        };
        eprintln!("{body}");
    } else {
        let text = match typed {
            // Context added on top of a library error, then the error with its hint.
            Some(t) if e.downcast_ref::<CliError>().is_none() && e.chain().count() > 1 => {
                let ctx: Vec<String> = e
                    .chain()
                    .take_while(|c| {
                        c.downcast_ref::<coroot_rs::Error>().is_none()
                            && c.downcast_ref::<CliError>().is_none()
                    })
                    .map(|c| c.to_string())
                    .collect();
                if ctx.is_empty() {
                    t.to_string()
                } else {
                    format!("{}: {t}", ctx.join(": "))
                }
            }
            Some(t) => t.to_string(),
            None => format!("{e:#}"),
        };
        eprintln!("{} {text}", crate::output::red("error:"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

    #[test]
    fn exit_codes_survive_context() {
        let e: anyhow::Result<()> = Err(err(Kind::NotFound, "gone"));
        let e = e.context("while doing things").unwrap_err();
        assert_eq!(exit_code(&e), 5);
        assert_eq!(exit_code(&anyhow::anyhow!("plain")), 1);
    }

    #[test]
    fn library_errors_get_hints() {
        let e = anyhow::Error::new(coroot_rs::Error::new(
            coroot_rs::ErrorKind::NotFound,
            "application 'x' not found",
        ));
        assert_eq!(exit_code(&e), 5);
        assert_eq!(
            typed(&e).unwrap().message,
            "application 'x' not found (see `corust apps`)"
        );
        let e = anyhow::Error::new(coroot_rs::Error::new(coroot_rs::ErrorKind::Decode, "bad"));
        assert_eq!(exit_code(&e), 1);
        let e = anyhow::Error::new(coroot_rs::Error::new(
            coroot_rs::ErrorKind::ResponseTooLarge,
            "too big",
        ));
        assert_eq!(exit_code(&e), 10);
        assert_eq!(typed(&e).unwrap().kind, Kind::ResponseTooLarge);
    }
}
