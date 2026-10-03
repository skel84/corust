//! The error type returned by every fallible operation.

use std::fmt;

use serde::Serialize;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What went wrong, independent of the message text.
///
/// Match on this to decide how to react (retry, ask for credentials, show
/// "unknown" instead of "failed", ...). New kinds may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// Coroot rejected the request as invalid (bad PromQL, unknown filter, missing field),
    /// or the result was too large and the request must be narrowed.
    InvalidInput,
    /// Missing, rejected or expired credentials, or an API key is required.
    Auth,
    /// Authenticated, but the user's role does not allow this.
    Forbidden,
    /// The project, application, node, alert, incident or trace does not exist.
    NotFound,
    /// A name matched several objects; see [`Error::candidates`].
    Ambiguous,
    /// Coroot could not be reached, or the request timed out.
    Network,
    /// Coroot returned an error.
    Server,
    /// This Coroot version does not support the operation.
    Unsupported,
    /// Coroot answered with something this crate could not understand.
    Decode,
}

/// An error from talking to Coroot.
#[derive(Debug, thiserror::Error)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    candidates: Vec<String>,
    status: Option<u16>,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Error {
    /// An error of `kind` with a message for people.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
            candidates: Vec::new(),
            status: None,
            source: None,
        }
    }

    pub(crate) fn with_candidates(mut self, mut candidates: Vec<String>) -> Self {
        candidates.sort();
        self.candidates = candidates;
        self
    }

    pub(crate) fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub(crate) fn with_source(
        mut self,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// What went wrong, to match on.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The message, without the candidate list.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// For [`ErrorKind::Ambiguous`] and some [`ErrorKind::NotFound`] errors: the names that
    /// matched, or that would have been accepted. Sorted.
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// The HTTP status code, when the error came from an HTTP response.
    pub fn http_status(&self) -> Option<u16> {
        self.status
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::InvalidInput, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::NotFound, message)
    }

    pub(crate) fn decode(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Decode, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::decode(format!("unexpected response from Coroot: {e}")).with_source(e)
    }
}
