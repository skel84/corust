//! Health status.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Coroot's health status, ordered by severity: `Unknown < Ok < Info < Warning < Critical`.
///
/// `Unknown` means there is no data to judge by. It is not a failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    /// No data to judge (also what Coroot reports for checks that do not apply).
    #[default]
    Unknown,
    /// Healthy.
    Ok,
    /// Worth knowing, not a problem.
    Info,
    /// Degraded.
    Warning,
    /// Broken or failing its SLO.
    Critical,
}

impl Status {
    /// The lowercase name: `unknown`, `ok`, `info`, `warning` or `critical`.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unknown => "unknown",
            Status::Ok => "ok",
            Status::Info => "info",
            Status::Warning => "warning",
            Status::Critical => "critical",
        }
    }

    /// Parses Coroot's status names; anything else is `Unknown`.
    pub fn parse(s: &str) -> Status {
        match s {
            "ok" => Status::Ok,
            "info" => Status::Info,
            "warning" => Status::Warning,
            "critical" => Status::Critical,
            _ => Status::Unknown,
        }
    }

    /// Warning or critical.
    pub fn is_problem(self) -> bool {
        self >= Status::Warning
    }

    /// Whether this is [`Status::Unknown`].
    pub fn is_unknown(&self) -> bool {
        *self == Status::Unknown
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Status {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        Ok(s.as_deref().map(Status::parse).unwrap_or_default())
    }
}
