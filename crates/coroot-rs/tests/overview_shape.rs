//! `Project::applications` and `Project::service_map` against a fake Coroot: a response
//! without the list fails with `ErrorKind::Decode` instead of passing for "no data", while
//! the empty forms Coroot really sends stay successful and HTTP/Coroot errors keep their
//! kinds.

use std::time::Duration;

use coroot_rs::{AppId, Client, ErrorKind, Project, Status};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Starts a fake Coroot that answers every request with `body` (HTTP 200); returns a
/// project on it.
async fn project_answering(body: &str) -> Project {
    project_answering_with("200 OK", "application/json", body).await
}

async fn project_answering_with(status: &str, content_type: &str, body: &str) -> Project {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (status, content_type, body) = (
        status.to_string(),
        content_type.to_string(),
        body.to_string(),
    );
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (status, content_type, body) = (status.clone(), content_type.clone(), body.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body.as_bytes()).await;
            });
        }
    });
    Client::builder(format!("http://{addr}"))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .project("p1")
}

fn envelope(data: Value) -> String {
    json!({"context": {"status": {"status": "ok"}}, "data": data}).to_string()
}

/// Bodies that do not carry the list under `key`.
fn malformed(key: &str) -> Vec<String> {
    vec![
        // Not JSON, or no body at all.
        "".into(),
        "  \n".into(),
        "{".into(),
        "not json".into(),
        // Not a {context, data} envelope.
        "{}".into(),
        "null".into(),
        "[]".into(),
        json!({key: []}).to_string(),
        json!({"context": null, "data": {key: []}}).to_string(),
        // `data` of the wrong type.
        envelope(json!([])),
        envelope(json!("x")),
        // The list missing or of the wrong type.
        envelope(json!({})),
        envelope(json!({"other": []})),
        envelope(json!({key: {}})),
        envelope(json!({key: "x"})),
        envelope(json!({key: 1})),
        // Entries that are not objects.
        envelope(json!({key: [null]})),
        envelope(json!({key: [{"id": "c:_:Unknown:a"}, "b"]})),
    ]
}

/// The empty forms Coroot sends: an empty list, a nil slice (`null`), and `data: null`
/// when it has no data for the project yet.
fn empty(key: &str) -> Vec<String> {
    vec![
        envelope(json!({key: []})),
        envelope(json!({key: null})),
        envelope(Value::Null),
    ]
}

#[tokio::test]
async fn applications_reject_a_missing_or_malformed_list() {
    for body in malformed("applications") {
        let e = project_answering(&body)
            .await
            .applications()
            .await
            .expect_err(&body);
        assert_eq!(e.kind(), ErrorKind::Decode, "{body}: {e}");
        // Shape errors name the endpoint; invalid JSON fails earlier, in the JSON parser.
        if serde_json::from_str::<Value>(&body).is_ok() {
            assert!(e.message().contains("overview/applications"), "{e}");
        }
    }
}

#[tokio::test]
async fn applications_accept_the_empty_forms() {
    for body in empty("applications") {
        let apps = project_answering(&body).await.applications().await;
        assert!(apps.as_ref().is_ok_and(Vec::is_empty), "{body}: {apps:?}");
    }
}

#[tokio::test]
async fn applications_keep_signal_semantics() {
    let body = envelope(json!({"applications": [{
        "id": "c:prod:Deployment:api", "cluster": "prod", "category": "application",
        "status": "warning",
        "cpu": {"status": "ok", "value": ""},
        "memory": null,
        "errors": {"status": "warning", "value": "3%"},
    }]}));
    let apps = project_answering(&body).await.applications().await.unwrap();
    assert_eq!(apps.len(), 1);
    let app = &apps[0];
    assert_eq!(app.id, AppId::new("c:prod:Deployment:api"));
    assert_eq!(app.status, Status::Warning);
    // Empty and malformed signals are kept, with the leniency of the per-field parsing.
    assert_eq!(app.signals["cpu"].status, Status::Ok);
    assert_eq!(app.signals["memory"].status, Status::Unknown);
    assert_eq!(app.signals["errors"].value, "3%");
    assert!(!app.signals.contains_key("latency"));
}

#[tokio::test]
async fn service_map_rejects_a_missing_or_malformed_list() {
    for body in malformed("map") {
        let e = project_answering(&body)
            .await
            .service_map()
            .await
            .expect_err(&body);
        assert_eq!(e.kind(), ErrorKind::Decode, "{body}: {e}");
        // Shape errors name the endpoint; invalid JSON fails earlier, in the JSON parser.
        if serde_json::from_str::<Value>(&body).is_ok() {
            assert!(e.message().contains("overview/map"), "{e}");
        }
    }
}

#[tokio::test]
async fn service_map_accepts_the_empty_forms() {
    for body in empty("map") {
        let map = project_answering(&body).await.service_map().await;
        assert!(
            map.as_ref()
                .is_ok_and(|m| m.nodes.is_empty() && m.edges.is_empty()),
            "{body}: {map:?}"
        );
    }
}

#[tokio::test]
async fn service_map_reads_a_valid_map() {
    let body = envelope(json!({"map": [
        {"id": "c:_:Unknown:web", "cluster": "prod", "category": "application",
         "status": "ok", "downstreams": [], "upstreams": [
            {"id": "c:_:Unknown:db", "status": "ok", "weight": 2.0}
        ]},
        {"id": "c:_:Unknown:db", "cluster": "prod", "category": "database",
         "status": "ok", "upstreams": null, "downstreams": null},
    ]}));
    let map = project_answering(&body).await.service_map().await.unwrap();
    assert_eq!(map.nodes.len(), 2);
    assert_eq!(map.edges.len(), 1);
    assert_eq!(map.edges[0].from, AppId::new("c:_:Unknown:web"));
    assert_eq!(map.edges[0].to, AppId::new("c:_:Unknown:db"));
}

/// HTTP and Coroot-reported errors keep their kinds; they are not shape errors.
#[tokio::test]
async fn http_and_coroot_errors_keep_their_kinds() {
    let json = "application/json";
    let cases = [
        ("401 Unauthorized", json, "", ErrorKind::Auth),
        ("403 Forbidden", json, "", ErrorKind::Forbidden),
        // A 404 without a Coroot error message means the endpoint is unknown.
        ("404 Not Found", json, "", ErrorKind::Unsupported),
        (
            "404 Not Found",
            "text/plain",
            "project not found",
            ErrorKind::NotFound,
        ),
        ("500 Internal Server Error", json, "boom", ErrorKind::Server),
        (
            "502 Bad Gateway",
            "text/plain",
            "bad gateway",
            ErrorKind::Server,
        ),
        (
            "200 OK",
            json,
            r#"{"context": {"status": {"error": "clickhouse is down"}}, "data": null}"#,
            ErrorKind::Server,
        ),
        (
            "200 OK",
            "text/html",
            "<!doctype html><html></html>",
            ErrorKind::Unsupported,
        ),
    ];
    for (status, ct, body, kind) in cases {
        let p = project_answering_with(status, ct, body).await;
        let e = p.applications().await.expect_err(status);
        assert_eq!(e.kind(), kind, "applications, {status} {body}: {e}");
        let e = p.service_map().await.expect_err(status);
        assert_eq!(e.kind(), kind, "service_map, {status} {body}: {e}");
    }
}
