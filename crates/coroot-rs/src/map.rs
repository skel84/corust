//! The service map: applications and the connections between them.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, ErrorKind, Result};
use crate::id::AppId;
use crate::json::{self, arr, is_false, s};
use crate::project::{Project, match_app};
use crate::status::Status;

/// The service map as a graph. Built by [`Project::service_map`], then optionally narrowed
/// with [`ServiceMap::focus`] and [`ServiceMap::retain_problems`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceMap {
    /// The applications.
    pub nodes: Vec<MapNode>,
    /// The connections, worst status first.
    pub edges: Vec<MapEdge>,
}

/// An application on the service map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapNode {
    /// The application id (serialized as its parts: `id`, `cluster_id`, `namespace`, `kind`,
    /// `name`).
    #[serde(flatten)]
    pub id: AppId,
    /// The cluster's display name.
    pub cluster: String,
    /// Coroot's category: `application`, `database`, `monitoring`, `control-plane`, or one
    /// configured in the project.
    pub category: String,
    /// The application status.
    pub status: Status,
    /// Whether it is a custom application defined in the project settings.
    #[serde(default, skip_serializing_if = "is_false")]
    pub custom: bool,
    /// Display labels (`instances` as a number, spaces in keys replaced with `_`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, Value>,
    /// Indicator name (`slo`, `instances`, `cpu`, `memory`, `net`, `dns`, `logs`) to status.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub indicators: BTreeMap<String, Status>,
    /// Hops from the focused application, after [`ServiceMap::focus`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<usize>,
}

/// A connection from a client application to one of its dependencies.
///
/// The numbers are recovered from the values Coroot displays, so they are rounded
/// (except `rps`, which is exact when Coroot reports a link weight).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapEdge {
    /// The client.
    #[serde(with = "crate::id::as_string")]
    pub from: AppId,
    /// The dependency it calls.
    #[serde(with = "crate::id::as_string")]
    pub to: AppId,
    /// The worst status of the connection checks.
    pub status: Status,
    /// Requests per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rps: Option<f64>,
    /// Average latency in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_seconds: Option<f64>,
    /// Traffic as seen by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_bytes_per_second: Option<f64>,
    /// Traffic as seen by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_bytes_per_second: Option<f64>,
    /// A problem with the connection, e.g. `connection errors`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub issue: String,
}

/// Which connections to follow from the focused application.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    /// What the application calls.
    Dependencies,
    /// What calls the application.
    Clients,
    /// Both directions.
    #[default]
    Both,
}

/// Numbers parsed from the link stats Coroot renders (utils.FormatLinkStats), e.g.
/// `["📈 0.4 rps ⏱️ 0.6ms", "↑229B/s ↓2kB/s"]` or `["⚠️ connection errors"]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LinkStats {
    pub rps: Option<f64>,
    pub latency_seconds: Option<f64>,
    pub sent_bytes_per_second: Option<f64>,
    pub received_bytes_per_second: Option<f64>,
    pub issue: String,
}

impl LinkStats {
    /// `weight` carries the exact request rate.
    pub fn parse(lines: &[String], weight: Option<f64>) -> LinkStats {
        let mut st = LinkStats::default();
        for line in lines {
            if let Some(issue) = line.strip_prefix("⚠️") {
                st.issue = issue.trim().to_string();
                continue;
            }
            let mut tokens = line.split_whitespace();
            while let Some(t) = tokens.next() {
                if t.starts_with('📈') {
                    let shown = tokens.next().and_then(|n| n.parse::<f64>().ok());
                    st.rps = weight.filter(|w| *w > 0.0).or(shown);
                } else if t.starts_with('⏱') {
                    st.latency_seconds = tokens.next().and_then(parse_latency);
                } else if let Some(r) = t.strip_prefix('↑') {
                    st.sent_bytes_per_second = parse_rate(r);
                } else if let Some(r) = t.strip_prefix('↓') {
                    st.received_bytes_per_second = parse_rate(r);
                }
            }
        }
        st
    }
}

/// Parses a latency rendered by Coroot's FormatLatency ("0.3ms", "<0.1ms", "2s").
fn parse_latency(s: &str) -> Option<f64> {
    let s = s.trim_start_matches('<');
    if let Some(ms) = s.strip_suffix("ms") {
        ms.parse::<f64>().ok().map(|v| v / 1000.0)
    } else {
        s.strip_suffix('s')?.parse().ok()
    }
}

/// Parses a rate rendered by Coroot's FormatBytes ("2kB/s", "30B/s", "0/s").
fn parse_rate(s: &str) -> Option<f64> {
    let s = s.strip_suffix("/s")?;
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let exp = ["", "B", "kB", "MB", "GB", "TB", "PB", "EB"]
        .iter()
        .position(|u| *u == unit)?
        .saturating_sub(1);
    Some(num.parse::<f64>().ok()? * 1000f64.powi(exp as i32))
}

fn edge(from: &str, l: &Value) -> MapEdge {
    let st = LinkStats::parse(&json::strings(l, "stats"), json::f(l, "weight"));
    MapEdge {
        from: AppId::new(from),
        to: AppId::new(s(l, "id")),
        status: Status::parse(s(l, "status")),
        rps: st.rps,
        latency_seconds: st.latency_seconds,
        sent_bytes_per_second: st.sent_bytes_per_second,
        received_bytes_per_second: st.received_bytes_per_second,
        issue: st.issue,
    }
}

fn node(a: &Value) -> MapNode {
    let labels = a
        .get("labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(k, v)| {
            let v = match (k.as_str(), v.as_str()) {
                ("instances", Some(n)) => n.parse::<i64>().map(Value::from).unwrap_or(v.clone()),
                _ => v.clone(),
            };
            (k.replace(' ', "_"), v)
        })
        .collect();
    let indicators = arr(a, "indicators")
        .iter()
        .map(|i| {
            (
                s(i, "message").to_lowercase(),
                Status::parse(s(i, "status")),
            )
        })
        .filter(|(k, _)| !k.is_empty())
        .collect();
    MapNode {
        id: AppId::new(s(a, "id")),
        cluster: s(a, "cluster").to_string(),
        category: s(a, "category").to_string(),
        status: Status::parse(s(a, "status")),
        custom: json::b(a, "custom"),
        labels,
        indicators,
        distance: None,
    }
}

impl ServiceMap {
    /// Builds the graph from Coroot's per-application upstream/downstream lists
    /// (the `data.map` array of the `overview/map` view).
    pub fn from_overview(apps: &[Value]) -> ServiceMap {
        let nodes = apps.iter().map(node).collect();
        let mut edges: Vec<MapEdge> = Vec::new();
        let mut seen = HashSet::new();
        for a in apps {
            for u in arr(a, "upstreams") {
                let e = edge(s(a, "id"), u);
                seen.insert((e.from.as_str().to_string(), e.to.as_str().to_string()));
                edges.push(e);
            }
        }
        // Downstream links mirror upstream ones; keep only those with no counterpart.
        for a in apps {
            for d in arr(a, "downstreams") {
                let key = (s(d, "id").to_string(), s(a, "id").to_string());
                if seen.insert(key.clone()) {
                    edges.push(MapEdge {
                        from: AppId::new(key.0),
                        to: AppId::new(key.1),
                        status: Status::Unknown,
                        rps: None,
                        latency_seconds: None,
                        sent_bytes_per_second: None,
                        received_bytes_per_second: None,
                        issue: String::new(),
                    });
                }
            }
        }
        let mut m = ServiceMap { nodes, edges };
        m.sort();
        m
    }

    /// Sorts nodes and edges by status (most severe first), then by id.
    pub fn sort(&mut self) {
        self.nodes
            .sort_by(|a, b| b.status.cmp(&a.status).then_with(|| a.id.cmp(&b.id)));
        self.edges.sort_by(|a, b| {
            b.status
                .cmp(&a.status)
                .then_with(|| (&a.from, &a.to).cmp(&(&b.from, &b.to)))
        });
    }

    /// The node of the application with this exact id.
    pub fn node(&self, id: &AppId) -> Option<&MapNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// Resolves a short name among the applications on the map; see
    /// [`Project::resolve_app`] for the rules.
    pub fn find(&self, query: &str) -> Result<AppId> {
        let ids: Vec<AppId> = self.nodes.iter().map(|n| n.id.clone()).collect();
        match_app(&ids, query).cloned().map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                Error::not_found(format!(
                    "application '{query}' has no connections on the service map"
                ))
            } else {
                e
            }
        })
    }

    /// Keeps the applications within `depth` hops of `app` in `direction`, and the edges
    /// along the traversal, and sets [`MapNode::distance`].
    pub fn focus(&mut self, app: &AppId, depth: usize, direction: Direction) {
        let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &self.edges {
            if direction != Direction::Clients {
                next.entry(e.from.as_str()).or_default().push(e.to.as_str());
            }
            if direction != Direction::Dependencies {
                next.entry(e.to.as_str()).or_default().push(e.from.as_str());
            }
        }
        let mut dist: HashMap<String, usize> = HashMap::from([(app.to_string(), 0)]);
        let mut queue = VecDeque::from([(app.to_string(), 0)]);
        while let Some((id, d)) = queue.pop_front() {
            if d == depth {
                continue;
            }
            for n in next.get(id.as_str()).into_iter().flatten() {
                if !dist.contains_key(*n) {
                    dist.insert(n.to_string(), d + 1);
                    queue.push_back((n.to_string(), d + 1));
                }
            }
        }
        self.nodes.retain(|n| dist.contains_key(n.id.as_str()));
        for n in &mut self.nodes {
            n.distance = dist.get(n.id.as_str()).copied();
        }
        // Only edges along the traversal, so a dependencies-only view has no client edges.
        self.edges.retain(
            |e| match (dist.get(e.from.as_str()), dist.get(e.to.as_str())) {
                (Some(f), Some(t)) => match direction {
                    Direction::Both => true,
                    Direction::Dependencies => *t == f + 1,
                    Direction::Clients => *f == t + 1,
                },
                _ => false,
            },
        );
    }

    /// Keeps problem edges (warning or critical, or with an issue), the applications they
    /// connect, applications with a problem status, and the focused application.
    pub fn retain_problems(&mut self) {
        self.edges
            .retain(|e| e.status.is_problem() || !e.issue.is_empty());
        let involved: HashSet<&str> = self
            .edges
            .iter()
            .flat_map(|e| [e.from.as_str(), e.to.as_str()])
            .collect();
        self.nodes.retain(|n| {
            n.status.is_problem() || involved.contains(n.id.as_str()) || n.distance == Some(0)
        });
    }
}

impl Project {
    /// The whole service map, sorted by status.
    pub async fn service_map(&self) -> Result<ServiceMap> {
        let env = self.get("overview/map", &[]).await?;
        Ok(ServiceMap::from_overview(json::arr_p(&env.data, "/map")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn link_stats() {
        let e = edge(
            "a",
            &json!({"id": "b", "status": "ok", "weight": 0.4321,
                    "stats": ["📈 0.4 rps ⏱️ 0.6ms", "↑229B/s ↓2kB/s"]}),
        );
        assert_eq!(e.rps, Some(0.4321));
        assert_eq!(e.latency_seconds, Some(0.0006));
        assert_eq!(e.sent_bytes_per_second, Some(229.0));
        assert_eq!(e.received_bytes_per_second, Some(2000.0));
        let e = edge(
            "a",
            &json!({"id": "b", "status": "critical", "stats": ["⚠️ connection errors"]}),
        );
        assert_eq!(e.issue, "connection errors");
        assert_eq!(e.rps, None);
        assert_eq!(parse_latency("<0.1ms"), Some(0.0001));
        assert_eq!(parse_latency("2s"), Some(2.0));
        assert_eq!(parse_rate("0/s"), Some(0.0));
    }

    #[test]
    fn graph_and_focus() {
        let apps = json!([
            {"id": "c:_:Unknown:web", "status": "warning", "upstreams": [{"id": "c:_:Unknown:db", "status": "ok"}],
             "downstreams": [{"id": "c:_:Unknown:lb"}]},
            {"id": "c:_:Unknown:db", "status": "ok", "upstreams": [], "downstreams": [{"id": "c:_:Unknown:web"}]},
            {"id": "c:_:Unknown:lb", "status": "ok", "upstreams": [{"id": "c:_:Unknown:web", "status": "ok"}], "downstreams": []},
            {"id": "c:_:Unknown:far", "status": "ok", "upstreams": [{"id": "c:_:Unknown:lb", "status": "ok"}], "downstreams": []},
        ]);
        let apps = apps.as_array().unwrap();
        let g = ServiceMap::from_overview(apps);
        assert_eq!(g.edges.len(), 3, "downstream links duplicate upstream ones");
        let web = g.find("web").unwrap();
        let mut deps = g.clone();
        deps.focus(&web, 1, Direction::Dependencies);
        assert_eq!(deps.edges.len(), 1);
        assert_eq!(deps.edges[0].to.name(), "db");
        let mut both = g.clone();
        both.focus(&web, 2, Direction::Both);
        assert_eq!(both.nodes.len(), 4);
        let far = both.nodes.iter().find(|n| n.id.name() == "far").unwrap();
        assert_eq!(far.distance, Some(2));
        let mut problems = both.clone();
        problems.retain_problems();
        assert!(problems.edges.is_empty());
        assert_eq!(problems.nodes.len(), 1, "the focused app stays");
        assert_eq!(
            g.find("nope").unwrap_err().message(),
            "application 'nope' has no connections on the service map"
        );

        let json = serde_json::to_value(&both).unwrap();
        let back: ServiceMap = serde_json::from_value(json).unwrap();
        assert_eq!(back.edges, both.edges);
    }
}
