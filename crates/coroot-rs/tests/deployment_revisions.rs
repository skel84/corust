//! `Project::deployment_revisions` against an invented `GET app/<id>` answer (Coroot 41180e8 shape).
use coroot_rs::{AppId, Client, ErrorKind, Project, Status};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const APP: &str = "c1:shop:Deployment:api";

/// Answers every request with `body`, recording each request line.
async fn project_answering(body: String) -> (Project, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (body, log) = (body.clone(), log.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&buf);
                log.lock()
                    .unwrap()
                    .push(line.lines().next().unwrap_or_default().to_string());
                let head = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body.as_bytes()).await;
            });
        }
    });
    let project = Client::builder(format!("http://{addr}"))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .project("p1");
    (project, seen)
}

fn response(data: Value) -> String {
    json!({"context": {"status": {"status": "ok"}}, "data": data}).to_string()
}

fn app_data() -> Value {
    json!({
        "app_map": {"application": {"id": APP}},
        "reports": [{
            "name": "Deployments", "status": "ok", "checks": [],
            "widgets": [{"table": {
                "header": ["Deployment", "Deployed", "Summary"],
                "rows": [{"id": "7b8e21:1790000000", "cells": [
                    {"value": "7b8e21: example.test/acme/api:2", "status": "warning"},
                    {"value": "1h ago"},
                    {"value": "", "is_stub": false, "deployment_summaries": [
                        {"report": "SLO", "ok": false, "message": "Availability: 98% (objective: 99%)", "time": null}]}]}]}}]
        }]
    })
}

#[tokio::test]
async fn reads_revisions_from_the_app_route() {
    let (project, seen) = project_answering(response(app_data())).await;
    let revs = project
        .deployment_revisions(&AppId::new(APP))
        .await
        .unwrap();
    assert_eq!(revs.len(), 1);
    assert_eq!(revs[0].id, "7b8e21:1790000000");
    assert_eq!(revs[0].status, Status::Warning);
    assert_eq!(revs[0].findings[0].report, "SLO");
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("GET /api/project/p1/app/c1%3Ashop%3ADeployment%3Aapi"),
        "{}",
        requests[0]
    );
}

#[tokio::test]
async fn zero_bound_fails_before_any_request() {
    let (project, seen) = project_answering(response(app_data())).await;
    let e = project
        .deployment_revisions_with(&AppId::new(APP), 0)
        .await
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::InvalidInput);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn no_data_and_foreign_application_are_not_empty_answers() {
    for body in [
        response(Value::Null),
        response(
            json!({"app_map": {"application": {"id": "c1:shop:Deployment:other"}}, "reports": []}),
        ),
        "{".to_string(),
    ] {
        let (project, _) = project_answering(body.clone()).await;
        let e = project
            .deployment_revisions(&AppId::new(APP))
            .await
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Decode, "{body}");
    }
}
