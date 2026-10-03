//! Logs, from the node agent or OpenTelemetry, stored in ClickHouse.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, ErrorKind, Result};
use crate::id::AppId;
use crate::json::{self, arr, s};
use crate::project::Project;
use crate::util::encode_segment;

/// Where logs come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    /// Container logs collected by the node agent.
    Agent,
    /// Logs sent with OpenTelemetry.
    Otel,
}

/// A comparison in a [`LogFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterOp {
    /// Equal.
    #[serde(rename = "=")]
    Eq,
    /// Not equal.
    #[serde(rename = "!=")]
    NotEq,
    /// Regular expression match.
    #[serde(rename = "~")]
    Matches,
    /// Regular expression mismatch.
    #[serde(rename = "!~")]
    NotMatches,
    /// The message contains all words of the value.
    #[serde(rename = "contains")]
    Contains,
    /// The message does not contain all words of the value.
    #[serde(rename = "not contains")]
    NotContains,
}

/// A condition on a log field (`Severity`, `Message`, `TraceId`) or attribute
/// (`service.name`, `host.name`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogFilter {
    /// The field or attribute name.
    pub name: String,
    /// The comparison.
    pub op: FilterOp,
    /// The value, a regular expression, or words, depending on `op`.
    pub value: String,
}

impl LogFilter {
    /// A filter on `name`.
    pub fn new(name: impl Into<String>, op: FilterOp, value: impl Into<String>) -> Self {
        LogFilter {
            name: name.into(),
            op,
            value: value.into(),
        }
    }

    /// Parses `NAME=VALUE`, `NAME!=VALUE`, `NAME~REGEX` or `NAME!~REGEX`.
    pub fn parse(expr: &str) -> Result<LogFilter> {
        if let Some(pos) = expr.find(['!', '~', '='])
            && pos > 0
            && let Some((op, sym)) = [
                (FilterOp::NotMatches, "!~"),
                (FilterOp::NotEq, "!="),
                (FilterOp::Matches, "~"),
                (FilterOp::Eq, "="),
            ]
            .into_iter()
            .find(|(_, sym)| expr[pos..].starts_with(sym))
        {
            return Ok(LogFilter::new(
                expr[..pos].trim(),
                op,
                &expr[pos + sym.len()..],
            ));
        }
        Err(Error::invalid(format!(
            "invalid filter '{expr}': expected NAME=VALUE, NAME!=VALUE, NAME~REGEX or NAME!~REGEX"
        )))
    }
}

/// An opaque position in the log stream: pass it as [`LogQuery::since`] to get only newer
/// entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogCursor(String);

impl fmt::Display for LogCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Selects log entries. Build it with the chained methods:
///
/// ```
/// use coroot_rs::{AppId, LogQuery};
///
/// let q = LogQuery::new()
///     .app(AppId::new("c1:shop:Deployment:checkout"))
///     .severity("error")
///     .contains("timeout")
///     .limit(50);
/// ```
#[derive(Debug, Clone)]
pub struct LogQuery {
    /// `None` searches the whole project.
    pub app: Option<AppId>,
    /// `None` reads both sources.
    pub source: Option<LogSource>,
    /// Conditions. Those on different names are ANDed; on the same name, `=` and `~`
    /// conditions are ORed and negated ones ANDed.
    pub filters: Vec<LogFilter>,
    /// Maximum number of entries (default 100).
    pub limit: usize,
    /// Only entries after this position; see [`LogPage::cursor`].
    pub since: Option<LogCursor>,
}

impl Default for LogQuery {
    fn default() -> Self {
        LogQuery {
            app: None,
            source: None,
            filters: Vec::new(),
            limit: 100,
            since: None,
        }
    }
}

impl LogQuery {
    /// A query for the whole project, both sources, the newest 100 entries.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only this application's logs.
    #[must_use]
    pub fn app(mut self, app: impl Into<AppId>) -> Self {
        self.app = Some(app.into());
        self
    }

    /// Only logs from this source.
    #[must_use]
    pub fn source(mut self, source: LogSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Adds a severity (`debug`, `info`, `warning`, `error`, `fatal`, ...); several
    /// severities are ORed.
    #[must_use]
    pub fn severity(self, severity: &str) -> Self {
        self.filter(LogFilter::new(
            "Severity",
            FilterOp::Eq,
            severity.to_lowercase(),
        ))
    }

    /// Only messages containing all words of `text`.
    #[must_use]
    pub fn contains(self, text: impl Into<String>) -> Self {
        self.filter(LogFilter::new("Message", FilterOp::Contains, text))
    }

    /// Drops messages containing all words of `text`.
    #[must_use]
    pub fn excludes(self, text: impl Into<String>) -> Self {
        self.filter(LogFilter::new("Message", FilterOp::NotContains, text))
    }

    /// Only entries of this trace.
    #[must_use]
    pub fn trace_id(self, id: impl Into<String>) -> Self {
        self.filter(LogFilter::new("TraceId", FilterOp::Eq, id))
    }

    /// Adds a condition.
    #[must_use]
    pub fn filter(mut self, f: LogFilter) -> Self {
        self.filters.push(f);
        self
    }

    /// Returns at most `n` entries.
    #[must_use]
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = n;
        self
    }

    /// Only entries after `cursor`, to follow the stream.
    #[must_use]
    pub fn since(mut self, cursor: LogCursor) -> Self {
        self.since = Some(cursor);
        self
    }
}

/// A log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// When the entry was logged.
    #[serde(with = "json::rfc3339_millis")]
    pub timestamp: DateTime<Utc>,
    /// The application that logged it.
    #[serde(with = "crate::id::as_string")]
    pub app_id: AppId,
    /// The severity, as Coroot normalizes it (`info`, `warning`, `error`, ...).
    pub severity: String,
    /// The message.
    pub message: String,
    /// The trace id, for entries logged within a trace.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trace_id: String,
    /// Attributes, e.g. `pod.name`, `host.name`, `service.name`.
    pub attributes: BTreeMap<String, String>,
}

/// A page of log entries.
#[derive(Debug, Clone, Default)]
pub struct LogPage {
    /// Oldest first.
    pub entries: Vec<LogEntry>,
    /// Pass to [`LogQuery::since`] to continue after this page.
    pub cursor: Option<LogCursor>,
}

/// Messages grouped by their shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPattern {
    /// The severity of the sample.
    pub severity: String,
    /// Messages matching the pattern in the time window.
    pub count: u64,
    /// Coroot's id of the pattern.
    pub hash: String,
    /// One message of this shape.
    pub sample: String,
}

impl Project {
    async fn logs_view(&self, q: &LogQuery, view: &str) -> Result<Value> {
        let (path, mut query) = match &q.app {
            Some(id) => {
                let mut query = json!({"view": view, "filters": q.filters, "limit": q.limit});
                if let Some(src) = q.source {
                    query["source"] = json!(match src {
                        LogSource::Agent => "agent",
                        LogSource::Otel => "otel",
                    });
                }
                (format!("app/{}/logs", encode_segment(id.as_str())), query)
            }
            None => {
                let (agent, otel) = match q.source {
                    Some(LogSource::Agent) => (true, false),
                    Some(LogSource::Otel) => (false, true),
                    None => (true, true),
                };
                (
                    "overview/logs".to_string(),
                    json!({"view": view, "agent": agent, "otel": otel, "filters": q.filters, "limit": q.limit}),
                )
            }
        };
        if let Some(since) = &q.since {
            query["since"] = json!(since.0);
        }
        let env = self.get(&path, &[("query", query.to_string())]).await?;
        let data = if q.app.is_some() {
            env.data
        } else {
            env.data.get("logs").cloned().unwrap_or_default()
        };
        if !s(&data, "error").is_empty() {
            return Err(Error::new(ErrorKind::Server, s(&data, "error")));
        }
        if s(&data, "status") == "warning"
            && arr(&data, "entries").is_empty()
            && !s(&data, "message").is_empty()
        {
            return Err(Error::new(ErrorKind::Unsupported, s(&data, "message")));
        }
        Ok(data)
    }

    /// Log entries, the newest `limit` of the time window (or since the cursor).
    pub async fn logs(&self, q: &LogQuery) -> Result<LogPage> {
        let data = self.logs_view(q, "messages").await?;
        let mut entries: Vec<LogEntry> = arr(&data, "entries")
            .iter()
            .map(|e| LogEntry {
                timestamp: json::time_ms(json::i(e, "timestamp")).unwrap_or_default(),
                app_id: q
                    .app
                    .clone()
                    .unwrap_or_else(|| AppId::new(s(e, "application"))),
                severity: s(e, "severity").to_string(),
                message: s(e, "message").to_string(),
                trace_id: s(e, "trace_id").to_string(),
                attributes: e
                    .get("attributes")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| k.as_str() != "Cluster")
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect(),
            })
            .collect();
        entries.reverse();
        let cursor = Some(s(&data, "max_ts"))
            .filter(|c| !c.is_empty())
            .map(|c| LogCursor(c.to_string()))
            .or_else(|| q.since.clone());
        Ok(LogPage { entries, cursor })
    }

    /// Log patterns of one application, most frequent first.
    pub async fn log_patterns(&self, q: &LogQuery) -> Result<Vec<LogPattern>> {
        if q.app.is_none() {
            return Err(Error::invalid("log patterns need an application"));
        }
        let data = self.logs_view(q, "patterns").await?;
        let mut patterns: Vec<LogPattern> = arr(&data, "patterns")
            .iter()
            .map(|p| LogPattern {
                severity: s(p, "severity").to_string(),
                count: json::i(p, "sum").max(0) as u64,
                hash: s(p, "hash").to_string(),
                sample: s(p, "sample").to_string(),
            })
            .collect();
        patterns.sort_by_key(|p| std::cmp::Reverse(p.count));
        Ok(patterns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        assert_eq!(LogFilter::parse("host.name=a=b").unwrap().value, "a=b");
        assert_eq!(
            LogFilter::parse("service.name!~^api").unwrap().op,
            FilterOp::NotMatches
        );
        assert_eq!(LogFilter::parse("k!=v").unwrap().op, FilterOp::NotEq);
        assert_eq!(LogFilter::parse("a=b~c").unwrap().name, "a");
        assert!(LogFilter::parse("=v").is_err());
        assert!(LogFilter::parse("novalue").is_err());
        let q = LogQuery::new().severity("ERROR").excludes("health");
        assert_eq!(
            serde_json::to_value(&q.filters).unwrap(),
            json!([
                {"name": "Severity", "op": "=", "value": "error"},
                {"name": "Message", "op": "not contains", "value": "health"},
            ])
        );
    }
}
