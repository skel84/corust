//! Project health, deployments and risks.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::id::AppId;
use crate::json::{self, arr, s, sp};
use crate::project::Project;
use crate::status::Status;

/// The health of the project's integrations, and counts of what Coroot sees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatus {
    /// The project id.
    pub project_id: String,
    /// The overall status of the integrations.
    pub status: Status,
    /// What is wrong with the project's setup, when Coroot reports a problem.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// `None` when Prometheus is not configured.
    pub prometheus: Option<Component>,
    /// The node agent; `None` when Coroot has no information about it.
    pub node_agent: Option<NodeAgent>,
    /// kube-state-metrics, when Coroot checks it (Kubernetes clusters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kube_state_metrics: Option<Component>,
    /// Number of applications.
    pub applications: usize,
    /// Number of nodes.
    pub nodes: usize,
    /// Firing alerts by severity.
    pub alerts: BTreeMap<String, u64>,
    /// Open incidents by severity.
    pub incidents: BTreeMap<String, u64>,
}

/// The health of one integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    /// Whether the integration works.
    pub status: Status,
    /// What is wrong, when it does not.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// The health of coroot-node-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAgent {
    /// Whether node agents report data.
    pub status: Status,
    /// Nodes reporting data.
    pub nodes: i64,
}

/// A rollout Coroot detected, with its effect on the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deployment {
    /// The deployed application.
    pub app: AppId,
    /// The new version: an image tag or a revision.
    pub version: String,
    /// How the deployment went (`Critical` when it made things worse).
    pub status: Status,
    /// When the rollout started, when Coroot gives a link to it.
    #[serde(
        default,
        with = "json::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub started_at: Option<DateTime<Utc>>,
    /// What changed after the deployment.
    pub summary: Vec<DeploymentNote>,
}

/// A change after a deployment, e.g. "CPU usage increased by 30%".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentNote {
    /// An emoji, as Coroot renders it.
    pub status: String,
    /// The change, as a sentence.
    pub message: String,
}

/// A reliability or security risk, e.g. a single-instance database or an exposed port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Risk {
    /// The affected application.
    pub app: AppId,
    /// `Availability` or `Security`.
    pub category: String,
    /// The risk type, e.g. `single-instance-app`, `single-az-app`, `spot-only-app`,
    /// `unreplicated-database` or `db-internet-exposure`.
    #[serde(rename = "type")]
    pub kind: String,
    /// How serious Coroot considers it.
    pub severity: Status,
    /// What the risk is, as a sentence.
    pub description: String,
    /// For exposure risks: Coroot's details of what is reachable from where.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<Value>,
    /// Whether someone dismissed the risk in the UI.
    pub dismissed: bool,
}

fn component(v: Option<&Value>) -> Option<Component> {
    let v = v.filter(|v| v.is_object())?;
    let message = [s(v, "error"), s(v, "message")]
        .into_iter()
        .find(|m| !m.is_empty() && *m != "ok")
        .unwrap_or_default();
    Some(Component {
        status: Status::parse(s(v, "status")),
        message: message.to_string(),
    })
}

fn counts(v: Option<&Value>) -> BTreeMap<String, u64> {
    v.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_u64()?)))
        .collect()
}

impl Project {
    /// Integration health and object counts.
    pub async fn status(&self) -> Result<ProjectStatus> {
        let env = self.get("status", &[]).await?;
        let (d, c) = (&env.data, &env.context);
        Ok(ProjectStatus {
            project_id: self.id().to_string(),
            status: Status::parse(s(d, "status")),
            error: s(d, "error").to_string(),
            prometheus: component(d.get("prometheus")),
            node_agent: d
                .get("node_agent")
                .filter(|v| v.is_object())
                .map(|v| NodeAgent {
                    status: Status::parse(s(v, "status")),
                    nodes: json::i(v, "nodes"),
                }),
            kube_state_metrics: component(d.get("kube_state_metrics")),
            applications: json::arr_p(c, "/search/applications").len(),
            nodes: json::arr_p(c, "/search/nodes").len(),
            alerts: counts(c.get("alerts")),
            incidents: counts(c.get("incidents")),
        })
    }

    /// Every deployment Coroot keeps for the applications present in the time window,
    /// newest first. The window picks the applications, not the deployments: an
    /// application's older deployments are listed too. Coroot sends no id, and the start
    /// time is inferred from the link's window. For one application's revisions with a
    /// stable id, use [`Project::deployment_revisions`].
    pub async fn deployments(&self) -> Result<Vec<Deployment>> {
        let env = self.get("overview/deployments", &[]).await?;
        Ok(json::arr_p(&env.data, "/deployments")
            .iter()
            .map(|d| Deployment {
                app: AppId::new(sp(d, "/application/id")),
                version: s(d, "version").to_string(),
                status: Status::parse(s(d, "status")),
                // The UI link spans the deployment start ±30 minutes.
                started_at: d
                    .pointer("/link/query/from")
                    .and_then(Value::as_i64)
                    .and_then(|from| json::time_ms(from + 30 * 60 * 1000)),
                summary: arr(d, "summary")
                    .iter()
                    .map(|n| DeploymentNote {
                        status: s(n, "status").to_string(),
                        message: s(n, "message").to_string(),
                    })
                    .collect(),
            })
            .collect())
    }

    /// Detected risks, including dismissed ones.
    pub async fn risks(&self) -> Result<Vec<Risk>> {
        let env = self.get("overview/risks", &[]).await?;
        Ok(json::arr_p(&env.data, "/risks")
            .iter()
            .map(|r| Risk {
                app: AppId::new(s(r, "application_id")),
                category: sp(r, "/key/category").to_string(),
                kind: sp(r, "/key/type").to_string(),
                severity: Status::parse(s(r, "severity")),
                description: [
                    sp(r, "/availability/description"),
                    sp(r, "/exposure/description"),
                    s(r, "description"),
                ]
                .into_iter()
                .find(|d| !d.is_empty())
                .unwrap_or_default()
                .to_string(),
                exposure: r.get("exposure").filter(|e| !e.is_null()).cloned(),
                dismissed: r.get("dismissal").is_some_and(|d| !d.is_null()),
            })
            .collect())
    }
}
