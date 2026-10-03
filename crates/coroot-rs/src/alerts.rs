//! Alerts and alerting rules.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::id::AppId;
use crate::incidents::StateFilter;
use crate::json::{self, arr, s};
use crate::project::Project;
use crate::status::Status;
use crate::util::encode_segment;

/// Where an alert is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertState {
    /// Active, with notifications sent.
    Firing,
    /// Firing, but notifications are muted.
    Suppressed,
    /// Resolved, by Coroot or manually.
    Resolved,
}

/// Selects alerts. The default returns up to 100 firing alerts.
#[derive(Debug, Clone)]
pub struct AlertQuery {
    /// Only alerts of this application.
    pub app: Option<AppId>,
    /// `Open` means firing or suppressed.
    pub state: StateFilter,
    /// Full-text search, as in the UI.
    pub search: Option<String>,
    /// Maximum number of alerts (default 100).
    pub limit: usize,
}

impl Default for AlertQuery {
    fn default() -> Self {
        AlertQuery {
            app: None,
            state: StateFilter::Open,
            search: None,
            limit: 100,
        }
    }
}

/// An alert raised by an alerting rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    /// The alert id, for [`Project::alert`] and [`Project::update_alerts`].
    pub id: String,
    /// The rule that raised it.
    pub rule_id: String,
    /// The rule's name.
    pub rule_name: String,
    /// The application it is about.
    pub app: AppId,
    /// `Warning` or `Critical`.
    pub severity: Status,
    /// Firing, suppressed or resolved.
    pub state: AlertState,
    /// Whether notifications are muted (also set on resolved alerts that were suppressed).
    pub suppressed: bool,
    /// What is wrong, as a sentence.
    pub summary: String,
    /// When it started firing.
    #[serde(with = "json::rfc3339::option")]
    pub opened_at: Option<DateTime<Utc>>,
    /// When it was resolved; `None` while open.
    #[serde(
        default,
        with = "json::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Who resolved the alert manually.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolved_by: String,
    /// How long it fired (until now while open).
    #[serde(rename = "duration_seconds", with = "json::seconds")]
    pub duration: Duration,
    /// The inspection report the alert comes from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub report: String,
    /// Only from [`Project::alert`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<AlertDetail>,
    /// For log-pattern alerts: the pattern, as in [`LogPattern::hash`](crate::LogPattern::hash).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub log_pattern_hash: String,
}

/// A labeled value attached to an alert, e.g. a sample log message or the query that fired.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertDetail {
    /// The label.
    pub name: String,
    /// The value.
    pub value: String,
}

/// What to do with alerts; see [`Project::update_alerts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertAction {
    /// Resolve manually (sends resolve notifications).
    Resolve,
    /// Mute notifications.
    Suppress,
    /// Reopen resolved or suppressed alerts.
    Reopen,
}

impl AlertAction {
    /// The action name in Coroot's API: `resolve`, `suppress` or `reopen`.
    pub fn as_str(self) -> &'static str {
        match self {
            AlertAction::Resolve => "resolve",
            AlertAction::Suppress => "suppress",
            AlertAction::Reopen => "reopen",
        }
    }
}

pub(crate) fn alert(v: &Value, detailed: bool) -> Alert {
    let resolved = json::i(v, "resolved_at").max(json::i(v, "manually_resolved_at"));
    let suppressed = json::b(v, "suppressed");
    let state = if resolved > 0 {
        AlertState::Resolved
    } else if suppressed {
        AlertState::Suppressed
    } else {
        AlertState::Firing
    };
    Alert {
        id: s(v, "id").to_string(),
        rule_id: s(v, "rule_id").to_string(),
        rule_name: s(v, "rule_name").to_string(),
        app: AppId::new(s(v, "application_id")),
        severity: Status::parse(s(v, "severity")),
        state,
        suppressed,
        summary: s(v, "summary").to_string(),
        opened_at: json::time(v, "opened_at"),
        resolved_at: json::time_ms(resolved),
        resolved_by: s(v, "resolved_by").to_string(),
        // Coroot keeps counting for manually resolved alerts; use the actual resolution time.
        duration: if resolved > 0 {
            Duration::from_millis((resolved - json::i(v, "opened_at")).max(0) as u64 / 1000 * 1000)
        } else {
            json::millis(v, "duration")
        },
        report: s(v, "report").to_string(),
        details: if detailed {
            arr(v, "details")
                .iter()
                .map(|d| AlertDetail {
                    name: s(d, "name").to_string(),
                    value: s(d, "value").to_string(),
                })
                .collect()
        } else {
            Vec::new()
        },
        log_pattern_hash: s(v, "log_pattern_hash").to_string(),
    }
}

/// An alerting rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    /// The rule id.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The severity of the alerts it raises.
    pub severity: Status,
    /// Whether it is evaluated.
    pub enabled: bool,
    /// Whether it ships with Coroot (built-in rules can be disabled but not deleted).
    pub builtin: bool,
    /// Defined in the config file; cannot be changed through the API.
    pub readonly: bool,
    /// What the rule evaluates (`{"type": "check", "check": {...}}`, `promql`, `log_patterns`, ...).
    pub source: Value,
    /// Which applications the rule applies to.
    pub selector: Value,
    /// How long the condition must hold before the alert fires.
    #[serde(rename = "for_seconds", with = "json::seconds")]
    pub for_duration: Duration,
    /// How long the alert keeps firing after the condition clears.
    #[serde(rename = "keep_firing_for_seconds", with = "json::seconds")]
    pub keep_firing_for: Duration,
    /// Alerts it has firing now.
    pub firing_alerts: u64,
}

fn rule(v: &Value, counts: &BTreeMap<String, u64>) -> AlertRule {
    let id = s(v, "id").to_string();
    AlertRule {
        firing_alerts: counts.get(&id).copied().unwrap_or_default(),
        id,
        name: s(v, "name").to_string(),
        severity: Status::parse(s(v, "severity")),
        enabled: json::b(v, "enabled"),
        builtin: json::b(v, "builtin"),
        readonly: json::b(v, "readonly"),
        source: v.get("source").cloned().unwrap_or_default(),
        selector: v.get("selector").cloned().unwrap_or_default(),
        for_duration: Duration::from_secs((json::i(v, "for") / 1000).max(0) as u64),
        keep_firing_for: Duration::from_secs((json::i(v, "keep_firing_for") / 1000).max(0) as u64),
    }
}

fn rule_path(id: &str) -> String {
    format!("alerting-rules/{}", encode_segment(id))
}

impl Project {
    /// Alerts, most recent first.
    pub async fn alerts(&self, q: &AlertQuery) -> Result<Vec<Alert>> {
        let filtered = q.app.is_some() || q.state == StateFilter::Resolved;
        let mut query = vec![
            (
                "include_resolved",
                (q.state != StateFilter::Open).to_string(),
            ),
            (
                "limit",
                if filtered {
                    "1000".to_string()
                } else {
                    q.limit.to_string()
                },
            ),
        ];
        if let Some(search) = &q.search {
            query.push(("search", search.clone()));
        }
        let env = self.get("alerts", &query).await?;
        Ok(json::arr_p(&env.data, "/alerts")
            .iter()
            .filter(|a| {
                q.app
                    .as_ref()
                    .is_none_or(|id| s(a, "application_id") == id.as_str())
            })
            .map(|a| alert(a, false))
            .filter(|a| q.state != StateFilter::Resolved || a.state == AlertState::Resolved)
            .take(q.limit)
            .collect())
    }

    /// One alert with its details (samples, queries, ...).
    pub async fn alert(&self, id: &str) -> Result<Alert> {
        let env = self
            .get(&format!("alerts/{}", encode_segment(id)), &[])
            .await?;
        Ok(alert(&env.data, true))
    }

    /// Resolves, suppresses or reopens alerts. Requires the permission to edit alerts.
    pub async fn update_alerts<S: AsRef<str>>(&self, action: AlertAction, ids: &[S]) -> Result<()> {
        let ids: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
        if ids.is_empty() {
            return Err(Error::invalid("no alert ids given"));
        }
        self.send(
            Method::POST,
            &format!("alerts/{}", action.as_str()),
            Some(&json!({"ids": ids})),
        )
        .await?;
        Ok(())
    }

    /// Alerting rules, by name.
    pub async fn alert_rules(&self) -> Result<Vec<AlertRule>> {
        let env = self.get("alerting-rules", &[]).await?;
        let counts: BTreeMap<String, u64> = env
            .data
            .get("alert_counts")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_u64()?)))
            .collect();
        let mut rules: Vec<AlertRule> = arr(&env.data, "rules")
            .iter()
            .map(|r| rule(r, &counts))
            .collect();
        rules.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rules)
    }

    /// A rule as Coroot stores it: the document to edit and pass to
    /// [`Project::update_alert_rule`].
    pub async fn alert_rule(&self, id: &str) -> Result<Value> {
        let env = self.get(&rule_path(id), &[]).await?;
        if env.data.get("id").is_none() {
            return Err(Error::not_found(format!("rule '{id}' not found")));
        }
        Ok(env.data)
    }

    /// Creates a rule; returns what Coroot answers (usually the new rule).
    pub async fn create_alert_rule(&self, rule: &Value) -> Result<Option<Value>> {
        self.send(Method::POST, "alerting-rules", Some(rule)).await
    }

    /// Replaces a rule.
    pub async fn update_alert_rule(&self, id: &str, rule: &Value) -> Result<Option<Value>> {
        self.send(Method::PUT, &rule_path(id), Some(rule)).await
    }

    /// Deletes a rule and resolves its open alerts. Rules from the config file fail with
    /// [`ErrorKind::Forbidden`](crate::ErrorKind). Built-in rules cannot be deleted (disable
    /// them instead): Coroot resolves their alerts, then fails with a server error.
    pub async fn delete_alert_rule(&self, id: &str) -> Result<()> {
        self.send(Method::DELETE, &rule_path(id), None).await?;
        Ok(())
    }

    /// Enables or disables a rule. Returns the new state as Coroot reports it.
    pub async fn set_alert_rule_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let mut v = self.alert_rule(id).await?;
        v["enabled"] = json!(enabled);
        let updated = self.update_alert_rule(id, &v).await?.unwrap_or(v);
        Ok(json::b(&updated, "enabled"))
    }

    /// All rules as YAML, for Coroot's config file.
    pub async fn export_alert_rules(&self) -> Result<String> {
        let env = self.get("alerting-rules/export", &[]).await?;
        Ok(s(&env.data, "yaml").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_alert() {
        let v = json!({
            "id": "a1", "rule_id": "r", "rule_name": "Log errors", "application_id": "c:default:Deployment:api",
            "severity": "warning", "summary": "boom", "opened_at": 1_000_000, "resolved_at": null,
            "manually_resolved_at": 1_060_500, "suppressed": true, "resolved_by": "Admin", "duration": 999_000,
            "details": [{"name": "Sample", "value": "x", "code": true}],
        });
        let a = serde_json::to_value(alert(&v, true)).unwrap();
        assert_eq!(a["state"], "resolved");
        assert_eq!(a["suppressed"], true);
        assert_eq!(a["duration_seconds"], 60);
        assert_eq!(a["opened_at"], "1970-01-01T00:16:40Z");
        assert_eq!(a["app"]["namespace"], "default");
        assert_eq!(a["details"][0], json!({"name": "Sample", "value": "x"}));
        let firing =
            serde_json::to_value(alert(&json!({"opened_at": 1, "duration": 5000}), false)).unwrap();
        assert_eq!(firing["state"], "firing");
        assert_eq!(firing["duration_seconds"], 5);
        assert!(firing.get("resolved_at").is_none());
        assert!(firing.get("details").is_none());
    }
}
