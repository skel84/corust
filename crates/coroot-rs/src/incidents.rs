//! Incidents: SLO violations, with Coroot's root cause analysis.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::id::AppId;
use crate::json::{self, arr, s};
use crate::project::Project;
use crate::status::Status;
use crate::util::encode_segment;

/// Which items to return, by state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StateFilter {
    /// Open incidents; firing (including suppressed) alerts.
    #[default]
    Open,
    /// Resolved incidents; resolved alerts.
    Resolved,
    /// Everything.
    Any,
}

/// Whether an incident is still going on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IncidentState {
    /// The SLO is still violated.
    Open,
    /// The incident ended.
    Resolved,
}

/// Selects incidents. The default returns the 20 most recent open incidents.
#[derive(Debug, Clone)]
pub struct IncidentQuery {
    /// Only incidents of this application.
    pub app: Option<AppId>,
    /// Which incidents, by state.
    pub state: StateFilter,
    /// Maximum number of incidents.
    pub limit: usize,
}

impl Default for IncidentQuery {
    fn default() -> Self {
        IncidentQuery {
            app: None,
            state: StateFilter::Open,
            limit: 20,
        }
    }
}

/// An SLO incident: a period when an application violated its availability or latency
/// objective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    /// The incident key, for [`Project::incident`] and UI links.
    pub key: String,
    /// The affected application.
    pub app: AppId,
    /// The cluster's display name.
    pub cluster: String,
    /// `Warning` or `Critical`.
    pub severity: Status,
    /// Open or resolved.
    pub state: IncidentState,
    /// When it started; `None` if Coroot did not report it (serialized as `null`).
    #[serde(with = "json::rfc3339::option")]
    pub opened_at: Option<DateTime<Utc>>,
    /// When it ended; `None` while open.
    #[serde(
        default,
        with = "json::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_at: Option<DateTime<Utc>>,
    /// How long it lasted (until now while open).
    #[serde(rename = "duration_seconds", with = "json::seconds")]
    pub duration: Duration,
    /// Share of requests affected, in percent.
    pub impact_percent: f64,
    /// What happened, as a sentence (e.g. `high latency`).
    pub description: String,
    /// Coroot's root cause analysis, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rca: Option<Rca>,
    /// SLO impact and burn rates; only from [`Project::incident`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slo: Option<Slo>,
}

/// Root cause analysis. Lists carry only `status`, `summary` and `error`; the details come
/// from [`Project::incident`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rca {
    /// Analysis state as Coroot reports it (e.g. `ok`, `in_progress`, `failed`).
    pub status: String,
    /// A one-line summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// The root cause, in Markdown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root_cause: String,
    /// Suggested fixes, in Markdown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub immediate_fixes: String,
    /// The full analysis, in Markdown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detailed_analysis: String,
    /// Why the analysis failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Applications involved in the failure propagation, with their issues.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub propagation: Vec<Propagation>,
}

/// An application on the failure's propagation path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Propagation {
    /// The application.
    #[serde(with = "crate::id::as_string")]
    pub app_id: AppId,
    /// Its status during the incident.
    pub status: Status,
    /// What was wrong with it.
    pub issues: Vec<String>,
}

/// How the incident affected the SLOs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slo {
    /// Requests that failed while the incident was open, in percent; `None` when
    /// unknown (serialized as `null`).
    pub availability_impact_percent: Option<f64>,
    /// Requests slower than the latency objective, in percent; `None` when unknown
    /// (serialized as `null`).
    pub latency_impact_percent: Option<f64>,
    /// Burn rate conditions of the availability SLO.
    pub availability_burn_rates: Vec<BurnRate>,
    /// Burn rate conditions of the latency SLO.
    pub latency_burn_rates: Vec<BurnRate>,
}

/// A multi-window burn rate alert condition and its current values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurnRate {
    /// The severity the condition raises.
    pub severity: Status,
    /// The long window, e.g. one hour.
    #[serde(rename = "long_window_seconds", with = "json::seconds")]
    pub long_window: Duration,
    /// The short window, which confirms the burn is still going on.
    #[serde(rename = "short_window_seconds", with = "json::seconds")]
    pub short_window: Duration,
    /// The error budget burn rate over the long window (1 consumes the budget exactly
    /// in the SLO period).
    pub long_window_burn_rate: Option<f64>,
    /// The burn rate over the short window.
    pub short_window_burn_rate: Option<f64>,
    /// The burn rate both windows must exceed.
    pub threshold: Option<f64>,
}

fn burn_rates(v: &Value, key: &str) -> Vec<BurnRate> {
    arr(v, key)
        .iter()
        .map(|b| BurnRate {
            severity: Status::parse(s(b, "severity")),
            long_window: Duration::from_secs((json::i(b, "long_window") / 1000).max(0) as u64),
            short_window: Duration::from_secs((json::i(b, "short_window") / 1000).max(0) as u64),
            long_window_burn_rate: json::f(b, "long_window_burn_rate"),
            short_window_burn_rate: json::f(b, "short_window_burn_rate"),
            threshold: json::f(b, "threshold"),
        })
        .collect()
}

pub(crate) fn incident(v: &Value, detailed: bool) -> Incident {
    let detail = |key: &str, r: &Value| {
        if detailed {
            s(r, key).to_string()
        } else {
            String::new()
        }
    };
    let rca = v.get("rca").filter(|r| r.is_object()).map(|r| Rca {
        status: s(r, "status").to_string(),
        summary: s(r, "short_summary").to_string(),
        root_cause: detail("root_cause", r),
        immediate_fixes: detail("immediate_fixes", r),
        detailed_analysis: detail("detailed_root_cause_analysis", r),
        error: s(r, "error").to_string(),
        propagation: if detailed {
            json::arr_p(r, "/propagation_map/applications")
                .iter()
                .filter(|a| {
                    !arr(a, "issues").is_empty() || Status::parse(s(a, "status")).is_problem()
                })
                .map(|a| Propagation {
                    app_id: AppId::new(s(a, "id")),
                    status: Status::parse(s(a, "status")),
                    issues: json::strings(a, "issues"),
                })
                .collect()
        } else {
            Vec::new()
        },
    });
    let d = v.get("details").unwrap_or(&Value::Null);
    let pct = |key: &str| d.get(key).and_then(|x| json::f(x, "percentage"));
    let slo = detailed.then(|| Slo {
        availability_impact_percent: pct("availability_impact"),
        latency_impact_percent: pct("latency_impact"),
        availability_burn_rates: burn_rates(d, "availability_burn_rates"),
        latency_burn_rates: burn_rates(d, "latency_burn_rates"),
    });
    let resolved_at = json::time(v, "resolved_at");
    Incident {
        key: s(v, "key").to_string(),
        app: AppId::new(s(v, "application_id")),
        cluster: s(v, "cluster").to_string(),
        severity: Status::parse(s(v, "severity")),
        state: if resolved_at.is_some() {
            IncidentState::Resolved
        } else {
            IncidentState::Open
        },
        opened_at: json::time(v, "opened_at"),
        resolved_at,
        duration: json::millis(v, "duration"),
        impact_percent: json::f(v, "impact").unwrap_or_default(),
        description: s(v, "short_description").to_string(),
        rca,
        slo,
    }
}

impl Project {
    /// Incidents: open ones first, then the most recently opened (Coroot's order).
    pub async fn incidents(&self, q: &IncidentQuery) -> Result<Vec<Incident>> {
        // Filtering happens client-side, so fetch more than requested when filters are set.
        let fetch = if q.app.is_some() || q.state != StateFilter::Any {
            (q.limit * 10).max(500)
        } else {
            q.limit
        };
        let env = self
            .get("incidents", &[("limit", fetch.to_string())])
            .await?;
        Ok(env
            .data
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| {
                q.app
                    .as_ref()
                    .is_none_or(|a| s(i, "application_id") == a.as_str())
            })
            .filter(|i| {
                let resolved = json::i(i, "resolved_at") > 0;
                match q.state {
                    StateFilter::Open => !resolved,
                    StateFilter::Resolved => resolved,
                    StateFilter::Any => true,
                }
            })
            .take(q.limit)
            .map(|i| incident(i, false))
            .collect())
    }

    /// One incident with the full root cause analysis and SLO details.
    pub async fn incident(&self, key: &str) -> Result<Incident> {
        let env = self
            .get(&format!("incident/{}", encode_segment(key)), &[])
            .await?;
        Ok(incident(&env.data, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalized_incident() {
        let v = json!({
            "key": "k1", "application_id": "c:_:Unknown:db", "opened_at": 1_000_000, "resolved_at": null,
            "severity": "critical", "impact": 12.5, "short_description": "high latency", "duration": 120_000,
            "details": {"latency_burn_rates": [{"long_window": 3_600_000, "long_window_burn_rate": 14.4, "severity": "critical"}]},
            "rca": {"status": "ok", "short_summary": "disk", "root_cause": "full disk",
                    "propagation_map": {"applications": [{"id": "c:_:Unknown:db", "status": "critical", "issues": ["disk full"]},
                                                         {"id": "c:_:Unknown:ok", "status": "ok"}]}},
        });
        let brief = serde_json::to_value(incident(&v, false)).unwrap();
        assert_eq!(brief["state"], "open");
        assert_eq!(brief["duration_seconds"], 120);
        assert_eq!(brief["opened_at"], "1970-01-01T00:16:40Z");
        assert_eq!(brief["rca"], json!({"status": "ok", "summary": "disk"}));
        assert!(brief.get("slo").is_none());
        let full = serde_json::to_value(incident(&v, true)).unwrap();
        assert_eq!(full["rca"]["root_cause"], "full disk");
        assert_eq!(full["rca"]["propagation"].as_array().unwrap().len(), 1);
        assert_eq!(full["rca"]["propagation"][0]["app_id"], "c:_:Unknown:db");
        assert_eq!(
            full["slo"]["latency_burn_rates"][0]["long_window_seconds"],
            3600
        );
        let back: Incident = serde_json::from_value(full).unwrap();
        assert_eq!(back.duration, Duration::from_secs(120));
    }
}
