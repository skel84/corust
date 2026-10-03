//! A project handle: the entry point for reading a project's data.

use reqwest::Method;
use serde_json::{Map, Value, json};
use url::Url;

use crate::client::{Client, Envelope};
use crate::error::{Error, Result};
use crate::id::AppId;
use crate::mcp::McpSession;
use crate::nodes::NodeId;
use crate::time::TimeRange;
use crate::util::encode_segment;

/// One Coroot project, with the time window queries cover.
///
/// Cheap to clone. Get one from [`Client::project`] or [`Client::find_project`], then
/// narrow the window with [`Project::with_range`]:
///
/// ```no_run
/// # async fn example(client: coroot_rs::Client) -> coroot_rs::Result<()> {
/// use std::time::Duration;
/// use coroot_rs::TimeRange;
///
/// let project = client
///     .find_project("production")
///     .await?
///     .with_range(TimeRange::last(Duration::from_secs(6 * 3600)));
/// let firing = project.alerts(&Default::default()).await?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct Project {
    client: Client,
    id: String,
    range: TimeRange,
}

impl Project {
    pub(crate) fn new(client: Client, id: String, range: TimeRange) -> Self {
        Project { client, id, range }
    }

    /// The project id (not its name).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The client this project was opened with.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The time window queries use.
    pub fn range(&self) -> TimeRange {
        self.range
    }

    /// The same project with another time window.
    #[must_use]
    pub fn with_range(&self, range: TimeRange) -> Project {
        Project {
            range,
            ..self.clone()
        }
    }

    /// GET `api/project/<id>/<path>` with the time window, unwrapping the `{context, data}`
    /// envelope. For views this crate does not cover.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Envelope> {
        let mut q = self.range.query();
        q.extend(query.iter().cloned());
        self.client
            .get_envelope(&self.client.project_path(&self.id, path), &q)
            .await
    }

    /// Sends a request with an optional JSON body to `api/project/<id>/<path>`. Returns the
    /// response body as JSON, or `None` when it is empty or not JSON.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Option<Value>> {
        let v = self
            .client
            .send_json(method, &self.client.project_path(&self.id, path), body)
            .await?;
        Ok(match v {
            Value::Null => None,
            Value::Object(mut m) if m.contains_key("data") && m.contains_key("context") => {
                m.remove("data")
            }
            v => Some(v),
        })
    }

    /// Opens an MCP session with this project selected. Requires an API key.
    pub async fn mcp(&self) -> Result<McpSession> {
        McpSession::connect_project(self.client.clone(), &self.id).await
    }

    /// Calls an MCP tool in a short-lived session, adding the time window to the arguments.
    pub(crate) async fn call_tool(&self, tool: &str, args: Map<String, Value>) -> Result<Value> {
        let mut session = self.mcp().await?;
        let res = session.call_json(tool, self.mcp_args(args)).await;
        session.close().await;
        res
    }

    fn mcp_args(&self, mut args: Map<String, Value>) -> Value {
        if let Some(from) = self.range.from {
            args.insert("from".into(), json!(from.timestamp_millis().to_string()));
        }
        if let Some(to) = self.range.to {
            args.insert("to".into(), json!(to.timestamp_millis().to_string()));
        }
        Value::Object(args)
    }

    /// Link to a page of the Coroot UI for this project, with the time window.
    pub fn ui_url(&self, route: &str, query: &[(&str, String)]) -> Url {
        let mut u = self
            .client
            .base_url()
            .join(&format!(
                "p/{}/{}",
                encode_segment(&self.id),
                route.trim_start_matches('/')
            ))
            .unwrap_or_else(|_| self.client.base_url().clone());
        let mut q = self.range.query();
        q.extend(query.iter().cloned());
        if !q.is_empty() {
            u.query_pairs_mut()
                .extend_pairs(q.iter().map(|(k, v)| (*k, v.as_str())));
        }
        u
    }

    /// Link to a view: `applications`, `incidents`, `alerts`, `map`, `nodes`, `traces`,
    /// `logs`, `kubernetes`, `costs`, `anomalies`, `risks`, `dashboards`.
    pub fn view_url(&self, view: &str) -> Url {
        self.ui_url(view, &[])
    }

    /// Link to an application's page in the Coroot UI, for the project's time window.
    pub fn app_url(&self, app: &AppId) -> Url {
        self.ui_url(
            &format!("applications/{}", encode_segment(app.as_str())),
            &[],
        )
    }

    /// Link to a node's page in the Coroot UI.
    pub fn node_url(&self, node: &NodeId) -> Url {
        self.ui_url(&format!("nodes/{}", encode_segment(node.as_str())), &[])
    }

    /// Link to an incident in the Coroot UI.
    pub fn incident_url(&self, key: &str) -> Url {
        self.ui_url("incidents", &[("incident", key.to_string())])
    }

    /// Link to an alert in the Coroot UI.
    pub fn alert_url(&self, id: &str) -> Url {
        self.ui_url("alerts", &[("alert", id.to_string())])
    }

    /// The ids of all applications Coroot knows in this project.
    pub async fn application_ids(&self) -> Result<Vec<AppId>> {
        let env = self.get("status", &[]).await?;
        Ok(crate::json::arr_p(&env.context, "/search/applications")
            .iter()
            .filter_map(|x| x.get("id").and_then(Value::as_str))
            .map(AppId::new)
            .collect())
    }

    /// The ids of all nodes in this project.
    pub async fn node_ids(&self) -> Result<Vec<NodeId>> {
        let env = self.get("status", &[]).await?;
        Ok(crate::json::arr_p(&env.context, "/search/nodes")
            .iter()
            .map(|n| NodeId::new(crate::json::s(n, "cluster_id"), crate::json::s(n, "name")))
            .collect())
    }

    /// Resolves a short name to an application id. Accepted, from strictest to loosest:
    /// the full id, the id without the cluster, `namespace/name`, `Kind/name`, the name,
    /// and a substring of the id. Full ids are returned without a lookup.
    ///
    /// Fails with [`ErrorKind::Ambiguous`](crate::ErrorKind::Ambiguous) (candidates listed)
    /// when the first rule that matches matches several applications.
    pub async fn resolve_app(&self, query: &str) -> Result<AppId> {
        let id = AppId::new(query);
        if id.is_qualified() && !id.cluster_id().is_empty() && !query.contains('/') {
            return Ok(id);
        }
        let ids = self.application_ids().await?;
        if ids.is_empty() {
            return Err(Error::not_found(
                "no applications found in this project (has the node agent reported data yet?)",
            ));
        }
        match_app(&ids, query).cloned()
    }

    /// Resolves a node name (exact, then substring) or `cluster_id:name` to a node id.
    pub async fn resolve_node(&self, query: &str) -> Result<NodeId> {
        let nodes = self.node_ids().await?;
        crate::nodes::match_node(&nodes, query).cloned()
    }
}

/// Picks the best match for `query` among `ids`; see [`Project::resolve_app`].
pub fn match_app<'a>(ids: &'a [AppId], query: &str) -> Result<&'a AppId> {
    let q = query.trim();
    let ql = q.to_lowercase();
    type Rule<'r> = Box<dyn Fn(&AppId) -> bool + 'r>;
    let rules: Vec<Rule> = vec![
        Box::new(|id| id.as_str() == q),
        Box::new(|id| {
            id.as_str()
                .split_once(':')
                .is_some_and(|(_, rest)| rest == q)
        }),
        Box::new(|id| {
            let ns = id.as_str().split(':').nth(1).unwrap_or_default();
            format!("{ns}/{}", id.name()).to_lowercase() == ql
        }),
        Box::new(|id| format!("{}/{}", id.kind(), id.name()).to_lowercase() == ql),
        Box::new(|id| id.name().to_lowercase() == ql),
        Box::new(|id| id.as_str().to_lowercase().contains(&ql)),
    ];
    for rule in &rules {
        let found: Vec<&AppId> = ids
            .iter()
            .filter(|id| id.is_qualified() && rule(id))
            .collect();
        match found.as_slice() {
            [] => continue,
            [one] => return Ok(one),
            many => {
                return Err(Error::new(
                    crate::ErrorKind::Ambiguous,
                    format!("'{query}' matches several applications, be more specific:"),
                )
                .with_candidates(many.iter().map(|s| s.to_string()).collect()));
            }
        }
    }
    Err(Error::not_found(format!("application '{query}' not found")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn ids() -> Vec<AppId> {
        [
            "c1:default:Deployment:checkout",
            "c1:default:Deployment:checkout-worker",
            "c1:shop:Deployment:cart",
            "c1:db:StatefulSet:cart",
            "c1:_:Unknown:nginx",
        ]
        .into_iter()
        .map(AppId::new)
        .collect()
    }

    #[test]
    fn apps() {
        let ids = ids();
        let m = |q| match_app(&ids, q).map(|id| id.to_string());
        assert_eq!(m("checkout").unwrap(), "c1:default:Deployment:checkout");
        assert_eq!(
            m("default:Deployment:checkout").unwrap(),
            "c1:default:Deployment:checkout"
        );
        assert_eq!(m("shop/cart").unwrap(), "c1:shop:Deployment:cart");
        assert_eq!(m("StatefulSet/cart").unwrap(), "c1:db:StatefulSet:cart");
        assert_eq!(
            m("worker").unwrap(),
            "c1:default:Deployment:checkout-worker"
        );
        assert_eq!(m("NGINX").unwrap(), "c1:_:Unknown:nginx");
        let e = m("cart").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Ambiguous);
        assert_eq!(e.candidates().len(), 2);
        assert_eq!(m("nope").unwrap_err().kind(), ErrorKind::NotFound);
    }
}
