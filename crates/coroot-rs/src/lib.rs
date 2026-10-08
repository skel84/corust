//! An async client for [Coroot](https://coroot.com), the open-source observability platform.
//!
//! It reads what Coroot knows about a project (application health, the service map,
//! incidents with root cause analysis, alerts, logs, traces, metrics) and returns typed,
//! normalized data:
//!
//! - ids are parsed ([`AppId`], [`NodeId`]), statuses are an ordered enum ([`Status`]);
//! - timestamps are [`chrono::DateTime<Utc>`], durations [`std::time::Duration`];
//! - every model is `Serialize + Deserialize`, with a stable JSON form (snake_case keys,
//!   RFC 3339 times, `*_seconds` durations), so it can be cached, recorded as a fixture, or
//!   handed to another program as is;
//! - errors carry an [`ErrorKind`] to match on.
//!
//! # Example
//!
//! ```no_run
//! use std::time::Duration;
//! use coroot_rs::{Client, IncidentQuery, Status, TimeRange};
//!
//! # async fn example() -> coroot_rs::Result<()> {
//! let client = Client::builder("https://coroot.example.com")
//!     .api_key(std::env::var("COROOT_API_KEY").unwrap())
//!     .build()?;
//! let project = client
//!     .find_project("production")
//!     .await?
//!     .with_range(TimeRange::last(Duration::from_secs(3600)));
//!
//! for app in project.applications().await? {
//!     if app.status.is_problem() {
//!         println!("{} {}", app.status, app.id.short());
//!     }
//! }
//!
//! for incident in project.incidents(&IncidentQuery::default()).await? {
//!     let summary = incident.rca.as_ref().map(|r| r.summary.as_str()).unwrap_or("");
//!     println!("{} {} {}", incident.key, incident.app.short(), summary);
//! }
//!
//! // What breaks if checkout fails: everything that calls it, two hops out.
//! let checkout = project.resolve_app("checkout").await?;
//! let mut map = project.service_map().await?;
//! map.focus(&checkout, 2, coroot_rs::Direction::Clients);
//! # Ok(()) }
//! ```
//!
//! # Credentials
//!
//! Coroot accepts a user API key (`crt_...`, created in the UI under the user menu → API
//! keys) or the session cookie from an email/password login ([`ClientBuilder::login`]).
//! Some data comes from Coroot's MCP endpoint, which only accepts API keys: traces,
//! metrics, and the richer [`Project::app_health`]. Those methods say so.
//!
//! # Time windows
//!
//! A [`Project`] carries a [`TimeRange`]. The default leaves it to Coroot (the last hour).
//!
//! # Chart histories
//!
//! [`Project::app_charts`] decodes the chart widgets Coroot sends with an application's
//! reports (`GET app/<id>`). The server sends a time context (`from`, `to`, `step`) and bare
//! sample arrays: sample *i* is at `from + i * step`, as in Coroot's UI. It sends **no
//! units and no per-point timestamps**, NaN and infinite samples both arrive as `null`,
//! and a series' own start is not sent. Coverage is reported ([`SeriesCoverage`]), gaps stay
//! `None`, and history is limited to the project's time window. This crate never derives a
//! history from the summary values of [`Project::app_health`].
//!
//! # Coroot versions
//!
//! Tested with Coroot 1.14+. Operations an older server does not support fail with
//! [`ErrorKind::Unsupported`].

#![warn(missing_docs, missing_debug_implementations)]

mod client;
mod error;
mod id;
mod json;
mod mcp;
mod project;
mod status;
mod time;
pub mod util;

mod alerts;
mod apps;
mod charts;
mod incidents;
mod logs;
mod map;
mod metrics;
mod nodes;
mod overview;
mod traces;

pub use client::{Client, ClientBuilder, Credentials, Envelope, Payload, ProjectInfo, User};
pub use error::{Error, ErrorKind, Result};
pub use id::AppId;
pub use mcp::{Listing, McpSession, ToolOutput};
pub use project::{Project, match_app};
pub use status::Status;
pub use time::TimeRange;

pub use alerts::{Alert, AlertAction, AlertDetail, AlertQuery, AlertRule, AlertState};
pub use apps::{
    AppHealth, Application, Chart, ClientLink, Dependency, Issue, Latency, LogPatternSummary,
    Report, SeriesSummary, Signal,
};
pub use charts::{
    AppCharts, ChartAnnotation, ChartHistory, ChartLimits, ReportCharts, SeriesCoverage,
    SeriesHistory,
};
pub use incidents::{
    BurnRate, Incident, IncidentQuery, IncidentState, IncidentView, Propagation, Rca, Slo,
    SloObjective, StateFilter,
};
pub use logs::{
    FilterOp, LogCursor, LogEntry, LogFilter, LogPage, LogPattern, LogQuery, LogSource,
};
pub use map::{Direction, MapEdge, MapNode, ServiceMap};
pub use metrics::{MetricNames, MetricsQuery, QueryResult, Series};
pub use nodes::{Check, Node, NodeDetails, NodeId, NodeReport};
pub use overview::{Component, Deployment, DeploymentNote, NodeAgent, ProjectStatus, Risk};
pub use traces::{
    Span, SpanDetails, SpanEvent, SpanStats, SpanStatus, TraceError, TraceQuery, TraceSummary,
};

/// Re-exported for [`Client::request`] and [`Project::send`].
pub use reqwest::Method;
