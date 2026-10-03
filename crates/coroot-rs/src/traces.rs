//! Distributed traces (OpenTelemetry or eBPF), through Coroot's MCP tools.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::json::null_default;
use crate::mcp::Listing;
use crate::project::Project;

/// Narrows trace queries to one service and/or span name.
#[derive(Debug, Clone, Default)]
pub struct TraceQuery {
    /// `service.name`.
    pub service: Option<String>,
    /// e.g. `GET /cart`.
    pub span: Option<String>,
}

impl TraceQuery {
    fn args(&self) -> Map<String, Value> {
        let mut a = Map::new();
        if let Some(s) = &self.service {
            a.insert("service".into(), json!(s));
        }
        if let Some(s) = &self.span {
            a.insert("span".into(), json!(s));
        }
        a
    }
}

/// Request counts and latency by endpoint, busiest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceSummary {
    /// Totals over all endpoints; `None` when there are no traces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall: Option<SpanStats>,
    /// Per endpoint (service and span name), busiest first.
    #[serde(flatten)]
    pub endpoints: Listing<SpanStats>,
}

/// Request statistics of an endpoint, or of all of them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpanStats {
    /// The cluster's display name.
    #[serde(default)]
    pub cluster: String,
    /// The `service.name`; empty in overall totals.
    #[serde(default)]
    pub service_name: String,
    /// The span name, e.g. `GET /cart`; empty in overall totals.
    #[serde(default)]
    pub span_name: String,
    /// Requests in the time window.
    pub total: f64,
    /// Failed requests in the time window.
    pub failed: f64,
    /// p50, p95, p99 in seconds.
    #[serde(default, deserialize_with = "null_default")]
    pub duration_quantiles: Vec<f64>,
}

impl SpanStats {
    /// Failed requests, in percent.
    pub fn error_percent(&self) -> f64 {
        if self.total > 0.0 {
            self.failed / self.total * 100.0
        } else {
            0.0
        }
    }
}

/// Errors grouped by endpoint and message, with a sample trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceError {
    /// The cluster's display name.
    #[serde(default)]
    pub cluster: String,
    /// The `service.name`.
    pub service_name: String,
    /// The span name.
    pub span_name: String,
    /// Attributes that distinguish this group: the peer or host name, `http.route`,
    /// `db.system`, `db.operation`, `messaging.system`, `messaging.operation`.
    #[serde(default, deserialize_with = "null_default")]
    pub labels: BTreeMap<String, String>,
    /// A trace with this error, for [`Project::trace`].
    pub sample_trace_id: String,
    /// The error message of the sample.
    pub sample_error: String,
    /// Occurrences in the time window.
    pub count: f64,
}

/// A span of a trace. Times are as Coroot reports them: epoch milliseconds and
/// milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    /// The `service.name`.
    pub service: String,
    /// The trace id.
    pub trace_id: String,
    /// The span id.
    pub id: String,
    /// Empty for the root span.
    #[serde(default)]
    pub parent_id: String,
    /// The span name, e.g. `GET /cart` or `SELECT orders`.
    pub name: String,
    /// Start, in epoch milliseconds.
    pub timestamp: i64,
    /// In milliseconds.
    pub duration: f64,
    /// Whether the span failed.
    pub status: SpanStatus,
    /// The URL or database statement, when the span has one.
    #[serde(default)]
    pub details: SpanDetails,
    /// Span and resource attributes.
    #[serde(default, deserialize_with = "null_default")]
    pub attributes: BTreeMap<String, String>,
    /// Events, e.g. exceptions.
    #[serde(default, deserialize_with = "null_default")]
    pub events: Vec<SpanEvent>,
    /// The cluster's display name.
    #[serde(default)]
    pub cluster: String,
}

impl Span {
    /// The start time.
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        crate::json::time_ms(self.timestamp)
    }

    /// The duration.
    pub fn elapsed(&self) -> Duration {
        Duration::from_secs_f64(self.duration.max(0.0) / 1000.0)
    }
}

/// Whether a span failed, and why.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpanStatus {
    /// Set when the span status is an error.
    pub error: bool,
    /// The status message.
    #[serde(default)]
    pub message: String,
}

/// What the span did, extracted from its attributes: the URL of an HTTP call or the
/// statement of a database call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpanDetails {
    /// The URL or statement.
    #[serde(default)]
    pub text: String,
    /// Language for highlighting, e.g. `sql`.
    #[serde(default)]
    pub lang: String,
}

/// An event recorded in a span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    /// Epoch milliseconds.
    pub timestamp: i64,
    /// The event name, e.g. `exception`.
    pub name: String,
    /// The event attributes, e.g. `exception.message`.
    #[serde(default, deserialize_with = "null_default")]
    pub attributes: BTreeMap<String, String>,
}

impl Project {
    /// Request counts, error counts and latency quantiles by endpoint. Requires an API key.
    pub async fn traces_summary(&self, q: &TraceQuery) -> Result<TraceSummary> {
        let v = self.call_tool("traces_summary", q.args()).await?;
        if v.is_null() {
            return Ok(TraceSummary::default());
        }
        Ok(serde_json::from_value(v)?)
    }

    /// The most frequent errors by endpoint. Requires an API key.
    pub async fn trace_errors(&self, q: &TraceQuery) -> Result<Listing<TraceError>> {
        Listing::from_value(self.call_tool("traces_errors", q.args()).await?)
    }

    /// Where time goes in requests slower than `slower_than` (up to `up_to`), compared
    /// with the rest: a flame graph diff, as Coroot returns it. Requires an API key.
    pub async fn trace_outliers(
        &self,
        q: &TraceQuery,
        slower_than: Duration,
        up_to: Option<Duration>,
    ) -> Result<Value> {
        let mut a = q.args();
        a.insert(
            "dur_from".into(),
            json!(format!("{}ms", slower_than.as_millis())),
        );
        if let Some(to) = up_to {
            a.insert("dur_to".into(), json!(format!("{}ms", to.as_millis())));
        }
        self.call_tool("traces_outliers", a).await
    }

    /// All spans of a trace, earliest first. Requires an API key.
    pub async fn trace(&self, trace_id: &str) -> Result<Listing<Span>> {
        let mut a = Map::new();
        a.insert("trace_id".into(), json!(trace_id));
        let spans: Listing<Span> = Listing::from_value(self.call_tool("get_trace", a).await?)?;
        if spans.items.is_empty() {
            return Err(Error::not_found(format!(
                "trace {trace_id} not found in the selected time range"
            )));
        }
        Ok(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings() {
        let s: TraceSummary = serde_json::from_value(json!({
            "overall": {"cluster": "", "service_name": "", "span_name": "", "total": 10, "failed": 1, "duration_quantiles": [0.1, 0.2, 0.3]},
            "total": 1, "returned": 1, "truncated": false, "hint": "",
            "items": [{"cluster": "c", "service_name": "api", "span_name": "GET /", "total": 10, "failed": 1, "duration_quantiles": null}],
        }))
        .unwrap();
        assert_eq!(s.endpoints.items[0].error_percent(), 10.0);
        let empty = serde_json::to_value(TraceSummary::default()).unwrap();
        assert_eq!(empty, json!({"total": 0, "returned": 0, "items": []}));
        let bare: Listing<TraceError> = Listing::from_value(json!([])).unwrap();
        assert_eq!(bare.total, 0);
    }
}
