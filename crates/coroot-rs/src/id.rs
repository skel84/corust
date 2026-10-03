//! Application identifiers.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A Coroot application id: `cluster_id:namespace:Kind:name`, for example
/// `c1:shop:Deployment:checkout` or `c1:_:Unknown:nginx` (outside Kubernetes).
///
/// The name may contain colons (`external:external:ExternalService:api.example.com:443`).
/// Ids that do not have four parts are kept as they are, with the whole id as the name.
///
/// Serializes as `{"id", "name", "namespace", "kind", "cluster_id"}` (namespace omitted
/// outside Kubernetes); deserializes from that object or from the id string.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppId {
    raw: String,
    cluster_id: String,
    namespace: String,
    kind: String,
    name: String,
}

impl AppId {
    /// Parses an id. Never fails: see the type docs for ids without four parts.
    pub fn new(id: impl Into<String>) -> Self {
        let raw = id.into();
        let parts: Vec<&str> = raw.splitn(4, ':').collect();
        let (cluster_id, namespace, kind, name) = match parts.as_slice() {
            [c, ns, k, n] => (c.to_string(), ns.to_string(), k.to_string(), n.to_string()),
            _ => (String::new(), String::new(), String::new(), raw.clone()),
        };
        AppId {
            raw,
            cluster_id,
            namespace,
            kind,
            name,
        }
    }

    /// The full id, `cluster_id:namespace:Kind:name`, as Coroot uses it.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The id of the cluster (Coroot project) the application runs in.
    pub fn cluster_id(&self) -> &str {
        &self.cluster_id
    }

    /// The Kubernetes namespace; `None` outside Kubernetes (Coroot uses `_`).
    pub fn namespace(&self) -> Option<&str> {
        match self.namespace.as_str() {
            "" | "_" => None,
            ns => Some(ns),
        }
    }

    /// `Deployment`, `StatefulSet`, `Unknown` (containers outside Kubernetes), `ExternalService`, ...
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The application name: the workload name in Kubernetes, the container or service name
    /// elsewhere.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `namespace/name`, or `name` outside Kubernetes: how Coroot's UI shows applications.
    pub fn short(&self) -> String {
        match self.namespace() {
            Some(ns) => format!("{ns}/{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether the id has all four parts.
    pub fn is_qualified(&self) -> bool {
        self.raw.splitn(4, ':').count() == 4
    }

    /// Endpoints outside the monitored infrastructure.
    pub fn is_external(&self) -> bool {
        self.kind == "ExternalService"
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl fmt::Debug for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AppId({:?})", self.raw)
    }
}

impl FromStr for AppId {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(AppId::new(s))
    }
}

impl From<&str> for AppId {
    fn from(s: &str) -> Self {
        AppId::new(s)
    }
}

impl From<String> for AppId {
    fn from(s: String) -> Self {
        AppId::new(s)
    }
}

impl From<&AppId> for AppId {
    fn from(id: &AppId) -> Self {
        id.clone()
    }
}

impl AsRef<str> for AppId {
    fn as_ref(&self) -> &str {
        &self.raw
    }
}

impl Serialize for AppId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let ns = self.namespace();
        let mut m = s.serialize_map(Some(if ns.is_some() { 5 } else { 4 }))?;
        m.serialize_entry("id", &self.raw)?;
        m.serialize_entry("name", &self.name)?;
        if let Some(ns) = ns {
            m.serialize_entry("namespace", ns)?;
        }
        m.serialize_entry("kind", &self.kind)?;
        m.serialize_entry("cluster_id", &self.cluster_id)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for AppId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = AppId;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an application id string or an object with an \"id\" field")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<AppId, E> {
                Ok(AppId::new(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AppId, A::Error> {
                let mut id = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "id" {
                        id = Some(map.next_value::<String>()?);
                    } else {
                        map.next_value::<de::IgnoredAny>()?;
                    }
                }
                id.map(AppId::new)
                    .ok_or_else(|| de::Error::missing_field("id"))
            }
        }
        d.deserialize_any(V)
    }
}

/// Serializes an [`AppId`] as its id string.
pub(crate) mod as_string {
    use super::AppId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(id: &AppId, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(id.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<AppId, D::Error> {
        AppId::deserialize(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts() {
        let id = AppId::new("c1:_:Unknown:host:8080");
        assert_eq!(id.name(), "host:8080");
        assert_eq!(id.namespace(), None);
        assert_eq!(id.short(), "host:8080");
        assert_eq!(id.to_string(), "c1:_:Unknown:host:8080");
        let id = AppId::new("c1:shop:Deployment:cart");
        assert_eq!(id.short(), "shop/cart");
        assert!(id.is_qualified());
        let odd = AppId::new("whatever");
        assert_eq!(odd.name(), "whatever");
        assert!(!odd.is_qualified());
    }

    #[test]
    fn serde() {
        let id = AppId::new("c1:_:Unknown:nginx");
        let v = serde_json::to_value(&id).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"id": "c1:_:Unknown:nginx", "name": "nginx", "kind": "Unknown", "cluster_id": "c1"})
        );
        assert_eq!(serde_json::from_value::<AppId>(v).unwrap(), id);
        assert_eq!(
            serde_json::from_value::<AppId>(serde_json::json!("c1:_:Unknown:nginx")).unwrap(),
            id
        );
    }
}
