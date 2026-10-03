//! PromQL queries and metric discovery, through Coroot's MCP tools.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, json};

use crate::error::Result;
use crate::json::{is_false, null_default};
use crate::project::Project;

/// A PromQL range query over the project's time window.
#[derive(Debug, Clone)]
pub struct MetricsQuery {
    /// The PromQL expression.
    pub query: String,
    /// Widened by Coroot to at most ~120 points. Default: the scrape interval.
    pub step: Option<Duration>,
    /// Maximum number of series (Coroot caps it at 1000).
    pub limit: usize,
}

impl MetricsQuery {
    /// A query with the default step and a limit of 100 series.
    pub fn new(query: impl Into<String>) -> Self {
        MetricsQuery {
            query: query.into(),
            step: None,
            limit: 100,
        }
    }
}

/// The result of [`Project::query_metrics`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    /// The query as Coroot ran it.
    pub query: String,
    /// Start of the window, RFC 3339.
    pub from: String,
    /// End of the window, RFC 3339.
    pub to: String,
    /// The effective step.
    pub step_seconds: u64,
    /// Series the query returned.
    pub series_total: u64,
    /// Series in this result.
    pub series_returned: u64,
    /// Whether series were left out because of the limit or the size budget.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// How to narrow the query, when truncated.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    /// The series, each with one value per step.
    #[serde(default, deserialize_with = "null_default")]
    pub series: Vec<Series>,
}

/// One time series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    /// The series labels.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub labels: BTreeMap<String, String>,
    /// One value per step; `None` where there is no sample.
    #[serde(default, deserialize_with = "null_default")]
    pub values: Vec<Option<f64>>,
}

impl Series {
    /// The values that are present.
    pub fn present(&self) -> impl Iterator<Item = f64> + '_ {
        self.values.iter().flatten().copied()
    }

    /// The most recent value present.
    pub fn last(&self) -> Option<f64> {
        self.values.iter().rev().flatten().next().copied()
    }
}

/// The result of [`Project::metric_names`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricNames {
    /// The regular expression names were matched against.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Names that match.
    pub total: u64,
    /// Names in this result.
    pub returned: u64,
    /// Whether names were left out because of the limit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// The names, sorted.
    #[serde(default, deserialize_with = "null_default")]
    pub names: Vec<String>,
}

impl Project {
    /// Runs a PromQL range query. Requires an API key.
    pub async fn query_metrics(&self, q: &MetricsQuery) -> Result<QueryResult> {
        let mut a = Map::new();
        a.insert("query".into(), json!(q.query));
        a.insert("limit".into(), json!(q.limit));
        if let Some(step) = q.step {
            a.insert("step_seconds".into(), json!(step.as_secs().max(1)));
        }
        Ok(serde_json::from_value(
            self.call_tool("query_metrics", a).await?,
        )?)
    }

    /// Metric names matching an RE2 regular expression (`.+` for all). Requires an API key.
    pub async fn metric_names(&self, pattern: &str, limit: usize) -> Result<MetricNames> {
        let mut a = Map::new();
        a.insert("match".into(), json!(pattern));
        a.insert("limit".into(), json!(limit));
        Ok(serde_json::from_value(
            self.call_tool("list_metric_names", a).await?,
        )?)
    }
}
