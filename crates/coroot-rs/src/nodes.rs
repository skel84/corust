//! Nodes (hosts and VMs).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, ErrorKind, Result};
use crate::json::{self, arr, s};
use crate::project::Project;
use crate::status::Status;
use crate::util::encode_segment;

/// A node id: `cluster_id:name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    /// The id of node `name` in cluster `cluster_id`.
    pub fn new(cluster_id: &str, name: &str) -> Self {
        NodeId(format!("{cluster_id}:{name}"))
    }

    /// Wraps an id in the `cluster_id:name` form.
    pub fn from_raw(id: impl Into<String>) -> Self {
        NodeId(id.into())
    }

    /// The full id, `cluster_id:name`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of the cluster (Coroot project) the node belongs to.
    pub fn cluster_id(&self) -> &str {
        self.0.split_once(':').map(|(c, _)| c).unwrap_or_default()
    }

    /// The node name.
    pub fn name(&self) -> &str {
        self.0.split_once(':').map(|(_, n)| n).unwrap_or(&self.0)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A node (host or VM) and its current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// The parsed id.
    pub id: NodeId,
    /// The node name.
    pub name: String,
    /// The cluster id.
    pub cluster_id: String,
    /// The cluster's display name.
    pub cluster: String,
    /// `Warning` when the node stopped sending metrics.
    pub status: Status,
    /// Coroot's word for the status: `up`, or `down (no metrics)`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_message: String,
    /// Time since boot.
    #[serde(rename = "uptime_seconds", with = "json::seconds")]
    pub uptime: Duration,
    /// CPU usage, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// Memory usage, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_percent: Option<f64>,
    /// The operating system.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub os: String,
    /// The kernel version.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kernel: String,
    /// The cloud provider, when known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cloud_provider: String,
    /// The instance type (e.g. `m5.large`), when known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance_type: String,
    /// The availability zone, when known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub availability_zone: String,
    /// The node's IP addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ips: Vec<String>,
    /// The number of GPUs.
    #[serde(default, skip_serializing_if = "json::is_zero")]
    pub gpus: u64,
}

/// A node's inspection reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDetails {
    /// The node id.
    pub id: NodeId,
    /// The worst status of the reports.
    pub status: Status,
    /// The reports on the node page (currently one, `Node`).
    pub reports: Vec<NodeReport>,
}

/// A node inspection report: its checks and the latest chart values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeReport {
    /// The report name.
    pub name: String,
    /// The worst status of its checks.
    pub status: Status,
    /// The checks that ran.
    pub checks: Vec<Check>,
    /// The latest value of each chart series: chart title → series name → value.
    pub latest: BTreeMap<String, BTreeMap<String, f64>>,
}

/// One check in a report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// Coroot's check id, e.g. `CPUNode`.
    pub id: String,
    /// What the check verifies.
    pub title: String,
    /// The outcome.
    pub status: Status,
    /// The result when the check fails, e.g. `high CPU utilization of node-1`.
    pub message: String,
}

fn node(v: &Value) -> Node {
    let name = s(v, "name").to_string();
    let cluster_id = s(v, "cluster_id").to_string();
    Node {
        id: NodeId::new(&cluster_id, &name),
        name,
        cluster_id,
        cluster: s(v, "cluster_name").to_string(),
        status: Status::parse(json::sp(v, "/status/status")),
        status_message: json::sp(v, "/status/message").to_string(),
        uptime: Duration::from_secs((json::i(v, "uptime_ms") / 1000).max(0) as u64),
        cpu_percent: json::f(v, "cpu_percent"),
        memory_percent: json::f(v, "memory_percent"),
        os: s(v, "os").to_string(),
        kernel: s(v, "kernel_version").to_string(),
        cloud_provider: s(v, "cloud_provider").to_string(),
        instance_type: s(v, "instance_type").to_string(),
        availability_zone: s(v, "availability_zone").to_string(),
        ips: json::strings(v, "ips"),
        gpus: json::i(v, "gpus").max(0) as u64,
    }
}

/// Last non-null value of a chart series.
fn last_value(series: &Value) -> Option<f64> {
    arr(series, "data").iter().rev().find_map(Value::as_f64)
}

fn report(r: &Value) -> NodeReport {
    let mut latest = BTreeMap::new();
    for w in arr(r, "widgets") {
        let charts: Vec<&Value> = match (w.get("chart"), w.get("chart_group")) {
            (Some(c), _) if c.is_object() => vec![c],
            (_, Some(g)) if g.is_object() => arr(g, "charts").iter().collect(),
            _ => vec![],
        };
        for c in charts {
            let title = s(c, "title");
            let values: BTreeMap<String, f64> = arr(c, "series")
                .iter()
                .filter_map(|series| Some((s(series, "name").to_string(), last_value(series)?)))
                .collect();
            if !values.is_empty() && !title.is_empty() {
                latest.insert(title.to_string(), values);
            }
        }
    }
    NodeReport {
        name: s(r, "name").to_string(),
        status: Status::parse(s(r, "status")),
        checks: arr(r, "checks")
            .iter()
            .map(|c| Check {
                id: s(c, "id").to_string(),
                title: s(c, "title").to_string(),
                status: Status::parse(s(c, "status")),
                message: s(c, "message").to_string(),
            })
            .collect(),
        latest,
    }
}

/// Finds a node by `cluster_id:name`, exact name, then substring.
pub(crate) fn match_node<'a>(nodes: &'a [NodeId], query: &str) -> Result<&'a NodeId> {
    let ql = query.to_lowercase();
    let rules: [&dyn Fn(&NodeId) -> bool; 3] = [
        &|n| n.as_str() == query,
        &|n| n.name().to_lowercase() == ql,
        &|n| n.name().to_lowercase().contains(&ql),
    ];
    for rule in rules {
        let found: Vec<&NodeId> = nodes.iter().filter(|n| rule(n)).collect();
        match found.as_slice() {
            [] => continue,
            [one] => return Ok(one),
            many => {
                return Err(Error::new(
                    ErrorKind::Ambiguous,
                    format!("'{query}' matches several nodes, be more specific:"),
                )
                .with_candidates(many.iter().map(|n| n.to_string()).collect()));
            }
        }
    }
    Err(Error::not_found(format!("node '{query}' not found")))
}

impl Project {
    /// All nodes, most severe status first, then by id.
    pub async fn nodes(&self) -> Result<Vec<Node>> {
        let env = self.get("overview/nodes", &[]).await?;
        let mut nodes: Vec<Node> = json::arr_p(&env.data, "/nodes").iter().map(node).collect();
        nodes.sort_by(|a, b| b.status.cmp(&a.status).then_with(|| a.id.cmp(&b.id)));
        Ok(nodes)
    }

    /// A node's inspection reports, with the latest value of each chart.
    pub async fn node(&self, id: &NodeId) -> Result<NodeDetails> {
        let env = self
            .get(&format!("node/{}", encode_segment(id.as_str())), &[])
            .await?;
        let d = &env.data;
        Ok(NodeDetails {
            id: id.clone(),
            status: Status::parse(s(d, "status")),
            reports: arr(d, "reports").iter().map(report).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        let nodes = vec![NodeId::new("c1", "node-a"), NodeId::new("c1", "node-b")];
        assert_eq!(match_node(&nodes, "node-a").unwrap().as_str(), "c1:node-a");
        assert_eq!(
            match_node(&nodes, "c1:node-b").unwrap().as_str(),
            "c1:node-b"
        );
        assert_eq!(
            match_node(&nodes, "node").unwrap_err().kind(),
            ErrorKind::Ambiguous
        );
        assert_eq!(
            match_node(&nodes, "x").unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn reports() {
        let r = report(&serde_json::json!({
            "name": "CPU", "status": "ok",
            "checks": [{"id": "CPUNode", "title": "Node CPU", "status": "ok"}],
            "widgets": [{"chart": {"title": "usage", "series": [{"name": "user", "data": [1, 2, null]}]}}],
        }));
        assert_eq!(r.latest["usage"]["user"], 2.0);
        assert_eq!(r.checks[0].message, "");
    }
}
