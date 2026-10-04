//! Incidents: SLO violations, with Coroot's root cause analysis.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
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

/// An incident detail response, including the SLO presentation reported by Coroot.
/// The application owns selection; this value owns one server observation.
/// Charts and RCA widgets are not decoded by this API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentView {
    incident: Incident,
    availability: Option<SloObjective>,
    latency: Option<SloObjective>,
}

impl IncidentView {
    /// Incident identity, impact, burn rates and RCA evidence.
    pub fn incident(&self) -> &Incident {
        &self.incident
    }
    /// Availability objective, absent when Coroot did not report an SLI.
    pub fn availability(&self) -> Option<&SloObjective> {
        self.availability.as_ref()
    }
    /// Latency objective, absent when Coroot did not report an SLI.
    pub fn latency(&self) -> Option<&SloObjective> {
        self.latency.as_ref()
    }
}

/// Coroot's SLO objective and compliance text. These are server-rendered strings,
/// not percentages that can safely be parsed or recomputed from incident impact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloObjective {
    objective: String,
    compliance: String,
    violated: bool,
    latency_threshold_seconds: Option<f64>,
}
impl SloObjective {
    /// Objective text as reported by the server.
    pub fn objective(&self) -> &str {
        &self.objective
    }
    /// Compliance text as reported by the server.
    pub fn compliance(&self) -> &str {
        &self.compliance
    }
    /// Whether Coroot marked this objective violated.
    pub fn is_violated(&self) -> bool {
        self.violated
    }
    /// Latency threshold in seconds; absent for availability or an omitted value.
    pub fn latency_threshold_seconds(&self) -> Option<f64> {
        self.latency_threshold_seconds
    }
}

fn shape(field: &str) -> Error {
    Error::decode(format!(
        "unexpected incident response: invalid or missing {field}"
    ))
}

fn optional_object<'a>(v: &'a Value, field: &str) -> Result<Option<&'a Value>> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value)),
        _ => Err(shape(field)),
    }
}

fn optional_list<'a>(v: &'a Value, field: &str) -> Result<&'a [Value]> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(values)) if values.iter().all(Value::is_object) => Ok(values),
        _ => Err(shape(field)),
    }
}

fn validate_incident(v: &Value) -> Result<()> {
    for field in [
        "key",
        "application_id",
        "severity",
        "cluster",
        "short_description",
    ] {
        let value = v
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| shape(field))?;
        if matches!(field, "key" | "application_id") && value.is_empty() {
            return Err(shape(field));
        }
    }
    if v.get("impact").and_then(Value::as_f64).is_none() {
        return Err(shape("impact"));
    }
    if v.get("duration")
        .and_then(Value::as_i64)
        .is_none_or(|n| n < 0)
    {
        return Err(shape("duration"));
    }
    for field in ["opened_at", "resolved_at"] {
        match v.get(field) {
            Some(Value::Null) => {}
            Some(value)
                if value
                    .as_i64()
                    .is_some_and(|n| n >= 0 && (n == 0 || json::time_ms(n).is_some())) => {}
            _ => return Err(shape(field)),
        }
    }
    if let Some(details) = optional_object(v, "details")? {
        for field in ["availability_impact", "latency_impact"] {
            if let Some(impact) = optional_object(details, field)? {
                match impact.get("percentage") {
                    None | Some(Value::Null) => {}
                    Some(n) if n.is_number() => {}
                    _ => return Err(shape(field)),
                }
            }
        }
        for field in ["availability_burn_rates", "latency_burn_rates"] {
            for rate in optional_list(details, field)? {
                if rate.get("severity").and_then(Value::as_str).is_none() {
                    return Err(shape(field));
                }
                for window in ["long_window", "short_window"] {
                    if rate
                        .get(window)
                        .and_then(Value::as_i64)
                        .is_none_or(|n| n < 0)
                    {
                        return Err(shape(window));
                    }
                }
                for number in [
                    "long_window_burn_rate",
                    "short_window_burn_rate",
                    "threshold",
                ] {
                    match rate.get(number) {
                        None | Some(Value::Null) => {}
                        Some(n) if n.is_number() => {}
                        _ => return Err(shape(number)),
                    }
                }
            }
        }
    }
    if let Some(rca) = optional_object(v, "rca")? {
        for field in [
            "status",
            "short_summary",
            "root_cause",
            "immediate_fixes",
            "detailed_root_cause_analysis",
            "error",
        ] {
            if rca.get(field).is_some_and(|value| !value.is_string()) {
                return Err(shape(field));
            }
        }
        if let Some(map) = optional_object(rca, "propagation_map")? {
            for app in optional_list(map, "applications")? {
                if app
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                    || app.get("status").and_then(Value::as_str).is_none()
                {
                    return Err(shape("propagation application"));
                }
                match app.get("issues") {
                    None | Some(Value::Null) => {}
                    Some(Value::Array(issues)) if issues.iter().all(Value::is_string) => {}
                    _ => return Err(shape("propagation issues")),
                }
            }
        }
    }
    Ok(())
}

fn objective(v: &Value, field: &str, latency: bool) -> Result<Option<SloObjective>> {
    let Some(value) = optional_object(v, field)? else {
        return Ok(None);
    };
    let objective = value
        .get("objective")
        .and_then(Value::as_str)
        .ok_or_else(|| shape(field))?;
    let compliance = value
        .get("compliance")
        .and_then(Value::as_str)
        .ok_or_else(|| shape(field))?;
    let violated = value
        .get("violated")
        .and_then(Value::as_bool)
        .ok_or_else(|| shape(field))?;
    let threshold = match value.get("threshold") {
        None | Some(Value::Null) => None,
        Some(n) => Some(
            n.as_f64()
                .filter(|n| *n >= 0.)
                .ok_or_else(|| shape(field))?,
        ),
    };
    Ok(Some(SloObjective {
        objective: objective.into(),
        compliance: compliance.into(),
        violated,
        latency_threshold_seconds: latency.then_some(threshold).flatten(),
    }))
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
    let slo = (detailed && d.is_object()).then(|| Slo {
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
    /// This is a bounded sample, not a complete history or total count. State/app
    /// filters are applied to that sample. `data: null` means no world is available
    /// yet and returns an empty sample. Malformed collections fail with Decode.
    pub async fn incidents(&self, q: &IncidentQuery) -> Result<Vec<Incident>> {
        // Filtering happens client-side, so fetch more than requested when filters are set.
        let fetch = if q.app.is_some() || q.state != StateFilter::Any {
            q.limit.saturating_mul(10).max(500)
        } else {
            q.limit
        };
        if fetch > u32::MAX as usize {
            return Err(Error::invalid(
                "incident limit exceeds the server's u32 range",
            ));
        }
        let env = self
            .get("incidents", &[("limit", fetch.to_string())])
            .await?;
        if !env.context.is_object() {
            return Err(shape("{context, data} envelope"));
        }
        let items = match &env.data {
            Value::Null => return Ok(Vec::new()),
            Value::Array(items) => items,
            _ => return Err(shape("incident list")),
        };
        if items.len() > fetch {
            return Err(shape("incident list exceeds requested limit"));
        }
        for item in items {
            validate_incident(item)?;
        }
        let mut keys = std::collections::HashSet::new();
        if items.iter().any(|i| !keys.insert(s(i, "key"))) {
            return Err(shape("duplicate incident key"));
        }
        Ok(items
            .iter()
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
        Ok(self.incident_view(key).await?.incident)
    }

    /// One incident with Coroot's optional objective/compliance presentation.
    /// An unavailable world (`data: null`) is a Decode error, never an empty incident.
    /// The returned identity must match `key`. Existing [`Self::incident`] callers
    /// retain their normalized type and now receive the same response validation.
    pub async fn incident_view(&self, key: &str) -> Result<IncidentView> {
        if key.is_empty() {
            return Err(Error::invalid("incident key is empty"));
        }
        let env = self
            .get(&format!("incident/{}", encode_segment(key)), &[])
            .await?;
        if !env.context.is_object() {
            return Err(shape("{context, data} envelope"));
        }
        validate_incident(&env.data)?;
        if s(&env.data, "key") != key {
            return Err(shape("incident key does not match request"));
        }
        Ok(IncidentView {
            incident: incident(&env.data, true),
            availability: objective(&env.data, "availability_slo", false)?,
            latency: objective(&env.data, "latency_slo", true)?,
        })
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
