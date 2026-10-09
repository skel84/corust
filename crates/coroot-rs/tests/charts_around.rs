//! `Project::charts_around` against invented `GET app/<id>` answers (Coroot 41180e8 shape).
use chrono::DateTime;
use coroot_rs::{
    AppId, AroundRevision, ChartSplit, Client, DeploymentRevision, ErrorKind, Project,
    RevisionWindow, SideCoverage, Status, TimeRange,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const APP: &str = "c1:shop:Deployment:api";
/// The revision started at 1790000070 s; a one-minute window runs 1790000010..1790000130.
const STARTED: i64 = 1790000070;

/// Answers every request with `status` and `body`, recording each request line.
async fn project_answering(status: &str, body: String) -> (Project, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let status = status.to_string();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (status, body, log) = (status.clone(), body.clone(), log.clone());
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
                    "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
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
        .project("p1")
        // The project's own range must not leak into the window around the revision.
        .with_range(TimeRange::last(Duration::from_secs(3 * 3600)));
    (project, seen)
}

fn revision() -> DeploymentRevision {
    DeploymentRevision {
        id: format!("7b8e21:{STARTED}"),
        hash: "7b8e21".into(),
        started_at: DateTime::from_timestamp(STARTED, 0).unwrap(),
        version: "7b8e21: api:1.4.0".into(),
        status: Status::Ok,
        findings: Vec::new(),
        note: Some("No notable changes".into()),
    }
}

/// Five places 30 s apart from 1790000010 s; the window asked for ends at `to_ms`.
fn response(to_ms: i64, data: Value) -> String {
    json!({"context": {"status": {"status": "ok"}}, "data": {
        "app_map": {"application": {"id": APP}},
        "reports": [{
            "name": "SLO", "status": "ok", "checks": [],
            "widgets": [{"chart": {
                "ctx": {"from": 1790000010000_i64, "to": to_ms, "step": 30000,
                        "raw_step": 15000, "truncated": false},
                "title": "Requests, per second",
                "series": [{"name": "ok", "data": data}],
                "threshold": null,
                "annotations": [{"name": "deployment 7b8e21: api:1.4.0",
                                 "x1": STARTED * 1000, "x2": null, "icon": "deploy"}]}}]
        }]
    }})
    .to_string()
}

#[tokio::test]
async fn reads_the_window_around_the_start_and_splits_it() {
    let body = response(1790000130000, json!([1.0, null, 2.0, 2.5, 3.0]));
    let (project, seen) = project_answering("200 OK", body).await;
    let window = RevisionWindow::both(Duration::from_secs(60));
    let got = project
        .charts_around(&AppId::new(APP), &revision(), window)
        .await
        .unwrap();
    let AroundRevision::Charts(around) = got else {
        panic!("expected charts, got {got:?}");
    };
    let line = seen.lock().unwrap()[0].clone();
    assert!(line.contains("from=1790000010000"), "{line}");
    assert!(line.contains("to=1790000130000"), "{line}");
    assert_eq!(around.revision, format!("7b8e21:{STARTED}"));
    let (_, chart) = around.charts.charts().next().unwrap();
    let split = around.split(chart);
    assert_eq!(
        split,
        ChartSplit {
            before: 2,
            after: 3
        }
    );
    assert_eq!(
        chart.series[0].sides(&split),
        (
            Some(SideCoverage::Gaps {
                present: 1,
                expected: 2
            }),
            Some(SideCoverage::Full)
        )
    );
    assert!(!around.ends_early(chart));
}

#[tokio::test]
async fn a_window_past_the_newest_data_ends_early() {
    // Coroot cut the window at its newest data, 30 s after the start.
    let body = response(1790000100000, json!([1.0, 1.0, 2.0, 2.0]));
    let (project, _) = project_answering("200 OK", body).await;
    let got = project
        .charts_around(
            &AppId::new(APP),
            &revision(),
            RevisionWindow::both(Duration::from_secs(60)),
        )
        .await
        .unwrap();
    let AroundRevision::Charts(around) = got else {
        panic!("expected charts");
    };
    let (_, chart) = around.charts.charts().next().unwrap();
    assert!(around.ends_early(chart));
    assert_eq!(
        around.split(chart),
        ChartSplit {
            before: 2,
            after: 2
        }
    );
}

#[tokio::test]
async fn a_404_for_the_window_is_no_data_not_a_missing_application() {
    let (project, _) = project_answering("404 Not Found", "Application not found\n".into()).await;
    let got = project
        .charts_around(&AppId::new(APP), &revision(), RevisionWindow::default())
        .await
        .unwrap();
    let AroundRevision::NoData { revision, from, to } = got else {
        panic!("expected no data, got {got:?}");
    };
    assert_eq!(revision, format!("7b8e21:{STARTED}"));
    assert_eq!(from.timestamp(), STARTED - 1800);
    assert_eq!(to.timestamp(), STARTED + 1800);
}

#[tokio::test]
async fn refusals_and_unsupported_answers_stay_errors() {
    for (status, body, kind) in [
        (
            "403 Forbidden",
            "You are not allowed to view this application.",
            ErrorKind::Forbidden,
        ),
        // A bare 404 is a route this server doesn't have, not a window without data.
        (
            "404 Not Found",
            "404 page not found",
            ErrorKind::Unsupported,
        ),
        ("500 Internal Server Error", "", ErrorKind::Server),
    ] {
        let (project, _) = project_answering(status, body.into()).await;
        let e = project
            .charts_around(&AppId::new(APP), &revision(), RevisionWindow::default())
            .await
            .unwrap_err();
        assert_eq!(e.kind(), kind, "{status}");
    }
}

#[tokio::test]
async fn invalid_windows_fail_before_any_request() {
    let (project, seen) = project_answering("200 OK", String::new()).await;
    for window in [
        RevisionWindow::both(Duration::from_secs(30)),
        RevisionWindow::both(Duration::from_secs(13 * 3600)),
    ] {
        let e = project
            .charts_around(&AppId::new(APP), &revision(), window)
            .await
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
    }
    let e = project
        .charts_around(&AppId::new(""), &revision(), RevisionWindow::default())
        .await
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::InvalidInput);
    assert!(seen.lock().unwrap().is_empty());
}
