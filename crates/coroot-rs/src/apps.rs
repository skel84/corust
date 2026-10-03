//! Applications and their health.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::Envelope;
use crate::error::Result;
use crate::id::AppId;
use crate::json::{self, arr, is_zero, null_default, s};
use crate::map::LinkStats;
use crate::project::Project;
use crate::status::Status;
use crate::util::encode_segment;

const SIGNALS: [&str; 12] = [
    "errors",
    "latency",
    "upstreams",
    "instances",
    "restarts",
    "cpu",
    "memory",
    "disk_io_load",
    "disk_usage",
    "network",
    "dns",
    "logs",
];

/// An application with its health signals, as on Coroot's Applications page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Application {
    /// The application id (serialized as its parts: `id`, `cluster_id`, `namespace`, `kind`,
    /// `name`).
    #[serde(flatten)]
    pub id: AppId,
    /// The cluster's display name.
    pub cluster: String,
    /// `application`, `monitoring`, `control-plane`, ... (configurable in Coroot).
    pub category: String,
    /// Detected technology, e.g. `postgres`, `golang`, `nginx`.
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub app_type: String,
    /// The worst status of its signals.
    pub status: Status,
    /// Health signals that have a value or a non-ok status, by name: `errors`, `latency`,
    /// `upstreams`, `instances`, `restarts`, `cpu`, `memory`, `disk_io_load`,
    /// `disk_usage`, `network`, `dns`, `logs`.
    pub signals: BTreeMap<String, Signal>,
}

/// One health signal on the Applications page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// Its status.
    pub status: Status,
    /// As Coroot displays it, e.g. `5ms` or `3 restarts`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
}

fn application(v: &Value) -> Application {
    let mut signals = BTreeMap::new();
    for key in SIGNALS {
        let Some(p) = v.get(key) else { continue };
        let status = Status::parse(s(p, "status"));
        let value = s(p, "value");
        if !value.is_empty() || status >= Status::Info {
            signals.insert(
                key.to_string(),
                Signal {
                    status,
                    value: value.to_string(),
                },
            );
        }
    }
    Application {
        id: AppId::new(s(v, "id")),
        cluster: s(v, "cluster").to_string(),
        category: s(v, "category").to_string(),
        app_type: json::sp(v, "/type/name").to_string(),
        status: Status::parse(s(v, "status")),
        signals,
    }
}

/// An application's health: inspections with their issues, key metrics, and the status of
/// its connections. This is the shape of Coroot's MCP `get_application_status` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppHealth {
    /// The application id.
    #[serde(with = "crate::id::as_string")]
    pub id: AppId,
    /// The Kubernetes namespace.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// Key metrics. Only from MCP.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub vitals: Vec<SeriesSummary>,
    /// The worst status of the reports.
    pub status: Status,
    /// One per inspection area (SLO, Instances, CPU, Memory, Storage, Net, Logs, ...).
    #[serde(default, deserialize_with = "null_default")]
    pub reports: Vec<Report>,
    /// Applications this one calls.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub dependencies: Vec<Dependency>,
    /// Applications calling this one.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub clients: Vec<ClientLink>,
}

impl AppHealth {
    /// Issues across all reports, with the report name.
    pub fn issues(&self) -> impl Iterator<Item = (&str, &Issue)> {
        self.reports
            .iter()
            .flat_map(|r| r.issues.iter().map(move |i| (r.name.as_str(), i)))
    }
}

/// A metric reduced to a few numbers and a sparkline.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeriesSummary {
    /// What the series measures, e.g. `rps`, `latency_p95_seconds`, `cpu_cores`,
    /// `memory_bytes` (vitals) or the chart series name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Labels that tell series of the same chart apart.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub labels: BTreeMap<String, String>,
    /// The most recent value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<f64>,
    /// The minimum over the time window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// The maximum over the time window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// The average over the time window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg: Option<f64>,
    /// Evenly spaced points over the time window; `None` where there is no data.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub sparkline: Vec<Option<f64>>,
}

/// An inspection report, e.g. `SLO`, `CPU` or `Logs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// The report name.
    pub name: String,
    /// The worst status of its checks.
    pub status: Status,
    /// Checks that are not ok.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub issues: Vec<Issue>,
    /// Only from MCP.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub charts: Vec<Chart>,
    /// The most frequent log patterns (Logs report, MCP only).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub log_patterns: Vec<LogPatternSummary>,
}

/// A failed check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    /// Coroot's check id, e.g. `SLOAvailability`.
    pub id: String,
    /// What the check verifies.
    pub title: String,
    /// `Warning` or `Critical`.
    pub status: Status,
    /// The result, e.g. `the app is serving errors`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// A chart of a report, summarized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chart {
    /// The chart title.
    pub title: String,
    /// Its series.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub series: Vec<SeriesSummary>,
    /// Series left out to keep the result small.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub series_omitted: u64,
}

/// A frequent log pattern of the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPatternSummary {
    /// Coroot's id of the pattern.
    pub hash: String,
    /// The severity of its messages.
    pub severity: String,
    /// One message of this shape (at most 200 characters).
    pub sample: String,
    /// Messages in the time window.
    pub messages: u64,
}

/// A connection to an application this one calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// The dependency.
    #[serde(with = "crate::id::as_string")]
    pub id: AppId,
    /// The dependency's own health.
    #[serde(default, skip_serializing_if = "Status::is_unknown")]
    pub status: Status,
    /// The health of the connection (failed connections, retransmissions, ...).
    pub connectivity: Status,
    /// What is wrong with the connection.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub connectivity_message: String,
    /// Protocols seen on the connection, e.g. `http`, `postgres`.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protocols: Vec<String>,
    /// Network round-trip time, latest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_seconds: Option<f64>,
    /// Requests per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rps: Option<f64>,
    /// Failed requests per second, latest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors_per_sec: Option<f64>,
    /// Request latency, latest values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_seconds: Option<Latency>,
}

/// Latency statistics in seconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Latency {
    /// Average.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg: Option<f64>,
    /// Median.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50: Option<f64>,
    /// 95th percentile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95: Option<f64>,
    /// 99th percentile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p99: Option<f64>,
}

/// An application calling this one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientLink {
    /// The client.
    #[serde(with = "crate::id::as_string")]
    pub id: AppId,
    /// The client's own health.
    #[serde(default, skip_serializing_if = "Status::is_unknown")]
    pub status: Status,
    /// Requests per second from this client. Only from the REST view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rps: Option<f64>,
    /// Average latency seen by this client. Only from the REST view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_seconds: Option<f64>,
}

/// Builds an [`AppHealth`] from the REST application view (no vitals, charts or log patterns;
/// connection numbers come from the rounded values Coroot displays).
fn rest_health(id: &AppId, env: &Envelope) -> AppHealth {
    let d = &env.data;
    let reports = arr(d, "reports")
        .iter()
        .filter(|r| s(r, "status") != "unknown" || !arr(r, "checks").is_empty())
        .map(|r| Report {
            name: s(r, "name").to_string(),
            status: Status::parse(s(r, "status")),
            issues: arr(r, "checks")
                .iter()
                .filter(|c| Status::parse(s(c, "status")).is_problem())
                .map(|c| Issue {
                    id: s(c, "id").to_string(),
                    title: s(c, "title").to_string(),
                    status: Status::parse(s(c, "status")),
                    message: s(c, "message").to_string(),
                })
                .collect(),
            charts: Vec::new(),
            log_patterns: Vec::new(),
        })
        .collect();
    let dependencies = json::arr_p(d, "/app_map/dependencies")
        .iter()
        .map(|a| {
            let stats =
                LinkStats::parse(&json::strings(a, "link_stats"), json::f(a, "link_weight"));
            Dependency {
                id: AppId::new(s(a, "id")),
                status: Status::parse(s(a, "status")),
                connectivity: Status::parse(s(a, "link_status")),
                connectivity_message: [s(a, "link_status_reason"), stats.issue.as_str()]
                    .into_iter()
                    .find(|m| !m.is_empty())
                    .unwrap_or_default()
                    .to_string(),
                protocols: Vec::new(),
                rtt_seconds: None,
                rps: stats.rps,
                errors_per_sec: None,
                latency_seconds: stats.latency_seconds.map(|avg| Latency {
                    avg: Some(avg),
                    ..Default::default()
                }),
            }
        })
        .collect();
    let clients = json::arr_p(d, "/app_map/clients")
        .iter()
        .map(|a| {
            let stats =
                LinkStats::parse(&json::strings(a, "link_stats"), json::f(a, "link_weight"));
            ClientLink {
                id: AppId::new(s(a, "id")),
                status: Status::parse(s(a, "status")),
                rps: stats.rps,
                latency_seconds: stats.latency_seconds,
            }
        })
        .collect();
    AppHealth {
        id: id.clone(),
        namespace: id.namespace().unwrap_or_default().to_string(),
        vitals: Vec::new(),
        status: Status::parse(json::sp(d, "/app_map/application/status")),
        reports,
        dependencies,
        clients,
    }
}

impl Project {
    /// All applications, most severe status first, then by id.
    pub async fn applications(&self) -> Result<Vec<Application>> {
        let env = self.get("overview/applications", &[]).await?;
        let mut apps: Vec<Application> = json::arr_p(&env.data, "/applications")
            .iter()
            .map(application)
            .collect();
        apps.sort_by(|a, b| b.status.cmp(&a.status).then_with(|| a.id.cmp(&b.id)));
        Ok(apps)
    }

    /// An application's health. Uses MCP, which adds vitals, charts and log patterns, when
    /// the client has an API key; otherwise the REST view ([`Project::app_health_rest`]).
    pub async fn app_health(&self, app: &AppId) -> Result<AppHealth> {
        if !self.client().credentials().is_api_key() {
            return self.app_health_rest(app).await;
        }
        let mut args = Map::new();
        args.insert("app_id".into(), json!(app.as_str()));
        let v = self.call_tool("get_application_status", args).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// An application's health from the REST view, which works with any credentials.
    pub async fn app_health_rest(&self, app: &AppId) -> Result<AppHealth> {
        let env = self
            .get(&format!("app/{}", encode_segment(app.as_str())), &[])
            .await?;
        Ok(rest_health(app, &env))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_application() {
        let v = json!({
            "id": "c:prod:Deployment:api", "cluster": "main", "category": "application", "status": "warning",
            "type": {"name": "golang"},
            "errors": {"status": "warning", "value": "", "chart": [1, 2]},
            "latency": {"status": "ok", "value": "5ms"},
            "cpu": {"status": "unknown", "value": ""},
        });
        let app = application(&v);
        let r = serde_json::to_value(&app).unwrap();
        assert_eq!(r["id"], "c:prod:Deployment:api");
        assert_eq!(r["name"], "api");
        assert_eq!(r["namespace"], "prod");
        assert_eq!(r["type"], "golang");
        assert_eq!(
            r["signals"],
            json!({"errors": {"status": "warning"}, "latency": {"status": "ok", "value": "5ms"}})
        );
        let back: Application = serde_json::from_value(r).unwrap();
        assert_eq!(back.id, app.id);
        assert_eq!(back.signals, app.signals);
    }

    #[test]
    fn mcp_health_roundtrip() {
        let v = json!({
            "id": "c:_:Unknown:web", "status": "warning",
            "vitals": [{"name": "rps", "last": 1.5, "sparkline": [1, null, 2]}],
            "reports": [{"name": "SLO", "status": "warning", "issues": [{"id": "x", "title": "Latency", "status": "warning"}]}],
            "dependencies": [{"id": "c:_:Unknown:db", "connectivity": "ok", "rps": 2.5, "latency_seconds": {"avg": 0.001}}],
            "clients": null,
        });
        let h: AppHealth = serde_json::from_value(v).unwrap();
        assert_eq!(h.issues().count(), 1);
        assert_eq!(h.dependencies[0].status, Status::Unknown);
        let out = serde_json::to_value(&h).unwrap();
        assert!(out["dependencies"][0].get("status").is_none());
        assert!(out.get("clients").is_none());
        assert_eq!(out["vitals"][0]["sparkline"], json!([1.0, null, 2.0]));
    }

    #[test]
    fn rest_fallback() {
        let env = Envelope {
            context: Value::Null,
            data: json!({
                "app_map": {
                    "application": {"id": "c:_:Unknown:web", "status": "critical"},
                    "dependencies": [{"id": "c:_:Unknown:db", "status": "ok", "link_status": "critical",
                                      "link_weight": 3.25, "link_stats": ["📈 3 rps ⏱️ 2ms"]}],
                    "clients": [{"id": "c:_:Unknown:lb", "status": "unknown", "link_stats": ["📈 2 rps ⏱️ 1.5ms"]}],
                },
                "reports": [
                    {"name": "SLO", "status": "critical", "checks": [{"id": "SLOLatency", "title": "Latency", "status": "critical", "message": "slow"}, {"id": "ok", "status": "ok"}]},
                    {"name": "GPU", "status": "unknown", "checks": []},
                ],
            }),
        };
        let h = rest_health(&AppId::new("c:_:Unknown:web"), &env);
        assert_eq!(h.status, Status::Critical);
        assert_eq!(h.reports.len(), 1);
        assert_eq!(h.reports[0].issues.len(), 1);
        let d = &h.dependencies[0];
        assert_eq!(
            (d.status, d.connectivity, d.rps),
            (Status::Ok, Status::Critical, Some(3.25))
        );
        assert_eq!(d.latency_seconds.as_ref().unwrap().avg, Some(0.002));
        assert_eq!(h.clients[0].status, Status::Unknown);
        assert_eq!(h.clients[0].rps, Some(2.0));
    }
}
