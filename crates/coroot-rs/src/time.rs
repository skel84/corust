//! Time windows.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// The time window a query covers. The default leaves both ends to Coroot, which shows
/// the last hour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeRange {
    /// Start of the window; `None` lets Coroot pick (an hour before `to`).
    pub from: Option<DateTime<Utc>>,
    /// End of the window; `None` means now.
    pub to: Option<DateTime<Utc>>,
}

impl TimeRange {
    /// A window from explicit bounds; either may be left to Coroot.
    pub fn new(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> Self {
        TimeRange { from, to }
    }

    /// The last `d`, ending now.
    pub fn last(d: Duration) -> Self {
        TimeRange {
            from: Some(Utc::now() - d),
            to: None,
        }
    }

    /// A fixed window.
    pub fn between(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        TimeRange {
            from: Some(from),
            to: Some(to),
        }
    }

    /// Whether both bounds are left to Coroot (the last hour).
    pub fn is_default(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    /// `from`/`to` as epoch milliseconds, the form Coroot's API and UI links take.
    pub fn query(&self) -> Vec<(&'static str, String)> {
        let mut q = Vec::new();
        if let Some(f) = self.from {
            q.push(("from", f.timestamp_millis().to_string()));
        }
        if let Some(t) = self.to {
            q.push(("to", t.timestamp_millis().to_string()));
        }
        q
    }
}
