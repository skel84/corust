//! `Project::app_charts` against an invented `GET app/<id>` answer (Coroot 41180e8 shape).
use coroot_rs::{AppId, ChartLimits, Client, ErrorKind, Project, SeriesCoverage};
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
        }]
    })
}

#[tokio::test]
async fn reads_charts_from_the_app_route() {
    let (project, seen) = project_answering(response(app_data())).await;
    let charts = project.app_charts(&AppId::new(APP)).await.unwrap();
    let (report, chart) = charts.charts().next().unwrap();
    assert_eq!(report, "SLO");
    assert_eq!(chart.series[0].coverage, SeriesCoverage::Full);
    assert_eq!(chart.series[0].samples[1], None);
    assert_eq!(
        chart.point_time(3).unwrap().timestamp_millis(),
        1790000100000
    );
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("GET /api/project/p1/app/c1%3Ashop%3ADeployment%3Aapi"),
        "{}",
        requests[0]
    );
}

#[tokio::test]
async fn invalid_limits_fail_before_any_request() {
    let (project, seen) = project_answering(response(app_data())).await;
    let limits = ChartLimits {
        max_points: 0,
        ..ChartLimits::default()
    };
    let e = project
        .app_charts_with(&AppId::new(APP), limits)
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
        "[]".to_string(),
    ] {
        let (project, _) = project_answering(body.clone()).await;
        let e = project.app_charts(&AppId::new(APP)).await.unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Decode, "{body}");
    }
}
