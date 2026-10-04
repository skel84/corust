//! Incident endpoint contract verified against Coroot cc7c1bf.
use coroot_rs::{Client, ErrorKind, IncidentQuery, IncidentState, Project, StateFilter};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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

fn item() -> Value {
    json!({"key":"k1", "application_id":"c:ns:Deployment:api", "cluster":"Production",
        "severity":"critical", "opened_at":1790000000000_i64, "resolved_at":null,
        "impact":0.0, "duration":120000, "short_description":"SLO violation"})
}

#[tokio::test]
async fn rejects_malformed_collections_and_entries() {
    let mut no_impact = item();
    no_impact.as_object_mut().unwrap().remove("impact");
    let mut no_key = item();
    no_key["key"] = json!("");
    let bodies = vec![
        "".into(),
        "{".into(),
        "{}".into(),
        "null".into(),
        json!([item()]).to_string(),
        envelope(json!({})),
        envelope(json!(false)),
        envelope(json!([null])),
        envelope(json!([no_impact])),
        envelope(json!([no_key])),
        envelope(json!([item(), item()])),
    ];
    for body in bodies {
        let error = project_answering(&body)
            .await
            .incidents(&IncidentQuery::default())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Decode, "{body}");
    }
}

#[tokio::test]
async fn accepts_server_empty_forms_and_preserves_zero() {
    for data in [Value::Null, json!([])] {
        assert!(
            project_answering(&envelope(data))
                .await
                .incidents(&IncidentQuery::default())
                .await
                .unwrap()
                .is_empty()
        );
    }
    let rows = project_answering(&envelope(json!([item()])))
        .await
        .incidents(&IncidentQuery::default())
        .await
        .unwrap();
    assert_eq!(rows[0].impact_percent, 0.0);
    assert_eq!(rows[0].state, IncidentState::Open);
    assert!(rows[0].slo.is_none());
}

#[tokio::test]
async fn details_preserve_missing_objectives_and_reported_values() {
    let p = project_answering(&envelope(item())).await;
    let view = p.incident_view("k1").await.unwrap();
    assert!(view.availability().is_none());
    assert!(view.latency().is_none());
    assert!(view.incident().slo.is_none());
    let mut v = item();
    v["availability_slo"] = json!({"objective":"99.9% of requests should not fail", "compliance":"97.2%", "violated":true,"threshold":0});
    v["latency_slo"] = json!({"objective":"99% faster than 500ms", "compliance":"100%", "violated":false,"threshold":0.5});
    v["details"] = json!({"availability_impact":{"percentage":0},"availability_burn_rates":[{"severity":"critical","long_window":3600000,"short_window":300000,"long_window_burn_rate":null,"short_window_burn_rate":0,"threshold":6}]});
    v["rca"] = json!({"status":"FAILED","error":"source analysis unavailable"});
    let view = project_answering(&envelope(v))
        .await
        .incident_view("k1")
        .await
        .unwrap();
    assert_eq!(view.availability().unwrap().compliance(), "97.2%");
    assert!(view.availability().unwrap().is_violated());
    assert_eq!(
        view.availability().unwrap().latency_threshold_seconds(),
        None
    );
    assert_eq!(
        view.latency().unwrap().latency_threshold_seconds(),
        Some(0.5)
    );
    let rate = &view
        .incident()
        .slo
        .as_ref()
        .unwrap()
        .availability_burn_rates[0];
    assert_eq!(rate.threshold, Some(6.));
    assert_eq!(rate.long_window_burn_rate, None);
    assert_eq!(rate.short_window_burn_rate, Some(0.));
    assert_eq!(
        view.incident().rca.as_ref().unwrap().error,
        "source analysis unavailable"
    );
}

#[tokio::test]
async fn rejects_unavailable_mismatched_or_malformed_detail() {
    let mut mismatch = item();
    mismatch["key"] = json!("other");
    let mut wrong_rca = item();
    wrong_rca["rca"] = json!({"propagation_map":{"applications":[{"id":"c:ns:Deployment:api","status":"critical","issues":[42]}]}});
    let mut wrong_objective = item();
    wrong_objective["latency_slo"] = json!({"objective":"fast", "compliance":100, "violated":true});
    let mut wrong_rates = item();
    wrong_rates["details"] = json!({"latency_burn_rates":{}});
    for data in [
        Value::Null,
        json!([]),
        mismatch,
        wrong_rca,
        wrong_objective,
        wrong_rates,
    ] {
        let p = project_answering(&envelope(data)).await;
        assert_eq!(
            p.incident_view("k1").await.unwrap_err().kind(),
            ErrorKind::Decode
        );
        assert_eq!(
            p.incident("k1").await.unwrap_err().kind(),
            ErrorKind::Decode
        );
    }
}

#[tokio::test]
async fn filtering_is_bounded_and_resolved_state_consistent() {
    let mut resolved = item();
    resolved["key"] = json!("k2");
    resolved["resolved_at"] = json!(1790000120000_i64);
    let p = project_answering(&envelope(json!([item(), resolved]))).await;
    let rows = p
        .incidents(&IncidentQuery {
            state: StateFilter::Resolved,
            limit: 1,
            ..IncidentQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, IncidentState::Resolved);
    assert_eq!(rows[0].key, "k2");
}

#[tokio::test]
async fn http_and_context_errors_keep_their_kinds() {
    for (status, kind) in [
        ("401 Unauthorized", ErrorKind::Auth),
        ("403 Forbidden", ErrorKind::Forbidden),
        ("404 Not Found", ErrorKind::NotFound),
        ("500 Internal Server Error", ErrorKind::Server),
    ] {
        let p = project_answering_with(status, "application/json", "{}").await;
        assert_eq!(
            p.incidents(&IncidentQuery::default())
                .await
                .unwrap_err()
                .kind(),
            kind
        );
        assert_eq!(p.incident_view("k1").await.unwrap_err().kind(), kind);
    }
    let body = json!({"context":{"status":{"error":"incident not found"}},"data":null}).to_string();
    assert_eq!(
        project_answering(&body)
            .await
            .incident_view("k1")
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
}
