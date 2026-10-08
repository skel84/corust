//! One answer of `GET app/<id>` decoded by every decoder that reads it: the caller's own
//! read of the data, the chart histories and the deployment revisions, with one request.
use coroot_rs::{AppCharts, AppId, ChartLimits, Client, DeploymentRevision, ErrorKind, Project};
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
            "name": "SLO", "status": "warning", "checks": [],
            "widgets": [{"chart": {
                "ctx": {"from": 1790000010000_i64, "to": 1790000130000_i64, "step": 30000,
                        "raw_step": 15000, "truncated": false},
                "title": "Requests, per second",
                "series": [{"name": "ok", "data": [1.5, null, 2.5, 3.0, 4.0]}],
                "threshold": null, "annotations": null}}]
        }, {
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
async fn one_answer_feeds_every_decoder() {
    let app = AppId::new(APP);
    let (project, seen) = project_answering(response(app_data())).await;
    let env = project
        .get(
            &format!("app/{}", coroot_rs::util::encode_segment(APP)),
            &[],
        )
        .await
        .unwrap();
    // The caller's own read of the same answer.
    assert_eq!(env.data["app_map"]["application"]["id"], APP);
    assert_eq!(env.data["reports"].as_array().unwrap().len(), 2);
    let charts = AppCharts::from_envelope(&env, &app, &ChartLimits::default()).unwrap();
    let revisions = DeploymentRevision::list_from_envelope(&env, &app, 10).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);

    // The same as the calls that make their own request.
    assert_eq!(charts, project.app_charts(&app).await.unwrap());
    assert_eq!(revisions, project.deployment_revisions(&app).await.unwrap());
    assert_eq!(charts.charts().count(), 1);
    assert_eq!(revisions[0].id, "7b8e21:1790000000");
}

#[tokio::test]
async fn decoders_keep_the_contract_of_the_calls() {
    let app = AppId::new(APP);
    let (project, _) = project_answering(response(app_data())).await;
    let env = project
        .get(
            &format!("app/{}", coroot_rs::util::encode_segment(APP)),
            &[],
        )
        .await
        .unwrap();
    let zero = ChartLimits {
        max_charts: 0,
        ..ChartLimits::default()
    };
    let invalid = [
        AppCharts::from_envelope(&env, &app, &zero).unwrap_err(),
        AppCharts::from_envelope(&env, &AppId::new(""), &ChartLimits::default()).unwrap_err(),
        DeploymentRevision::list_from_envelope(&env, &app, 0).unwrap_err(),
        DeploymentRevision::list_from_envelope(&env, &AppId::new(""), 10).unwrap_err(),
    ];
    for e in invalid {
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
    }
    let other = AppId::new("c1:shop:Deployment:other");
    assert_eq!(
        AppCharts::from_envelope(&env, &other, &ChartLimits::default())
            .unwrap_err()
            .kind(),
        ErrorKind::Decode
    );
    assert_eq!(
        DeploymentRevision::list_from_envelope(&env, &other, 10)
            .unwrap_err()
            .kind(),
        ErrorKind::Decode
    );
}
