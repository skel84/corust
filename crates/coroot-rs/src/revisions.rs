//! Deployment revisions from an application's Deployments report.
//!
//! See [`Project::deployment_revisions`] for what Coroot exposes and what it does not.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::charts::{app_reports, check_app};
use crate::client::Envelope;
use crate::error::{Error, Result};
use crate::id::AppId;
use crate::json::{self, null_default, rfc3339::option as rfc3339_option};
use crate::project::Project;
use crate::status::Status;
use crate::util::encode_segment;

const HEADER: [&str; 3] = ["Deployment", "Deployed", "Summary"];
/// More findings than this on one revision cannot be a real Coroot answer.
const MAX_FINDINGS: usize = 256;

/// One deployment (revision) of an application, with Coroot's own assessment of it.
///
/// `id` is the only stable identity Coroot gives a revision: the row id of its Deployments
/// report, `<hash>:<start unix seconds>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentRevision {
    /// The stable revision id, `<hash>:<start unix seconds>`.
    pub id: String,
    /// The revision hash (the part of the id before the start time).
    pub hash: String,
    /// When the rollout started, from the id (second precision).
    #[serde(with = "crate::json::rfc3339_millis")]
    pub started_at: DateTime<Utc>,
    /// Coroot's label: the hash, followed by the container images when it knows them.
    pub version: String,
    /// Coroot's status for the revision. `Unknown` when it has too little data.
    pub status: Status,
    /// What the server found, as it worded it. See [`Project::deployment_revisions`] for
    /// what each finding was compared with.
    #[serde(default, deserialize_with = "null_default")]
    pub findings: Vec<RevisionFinding>,
    /// The server's explanation when it has no findings, verbatim, e.g.
    /// `No notable changes`, `Collecting data...` or a cancellation message. It is
    /// free text; its wording is not a stable contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One finding the server attached to a revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionFinding {
    /// The report the finding belongs to, e.g. `SLO`, `Instances`.
    pub report: String,
    /// Whether it is good news (an improvement or a met objective).
    pub ok: bool,
    /// The server's text, e.g. `Availability: 99.9% (objective: 99%)`.
    pub message: String,
    /// The revision's start time: the server stamps every summary finding with it.
    #[serde(
        default,
        with = "rfc3339_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub time: Option<DateTime<Utc>>,
}

fn shape(what: &str) -> Error {
    Error::decode(format!(
        "unexpected application response: invalid Deployments report ({what})"
    ))
}

fn revision(row: &Value) -> Result<DeploymentRevision> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| shape("row id"))?;
    let (hash, secs) = id.rsplit_once(':').ok_or_else(|| shape("row id"))?;
    let started_at = secs
        .parse::<i64>()
        .ok()
        .filter(|s| *s > 0)
        .and_then(|s| json::time_ms(s.checked_mul(1000)?))
        .filter(|_| !hash.is_empty())
        .ok_or_else(|| shape("row id"))?;
    let cells = row
        .get("cells")
        .and_then(Value::as_array)
        .filter(|c| c.len() == HEADER.len() && c.iter().all(Value::is_object))
        .ok_or_else(|| shape("cells"))?;
    let (version, summary) = (&cells[0], &cells[2]);
    let label = match version.get("value") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(shape("version cell")),
    };
    let status = match version.get("status") {
        None | Some(Value::Null) => Status::Unknown,
        Some(Value::String(s)) => Status::parse(s),
        _ => return Err(shape("version status")),
    };
    let findings = match summary.get("deployment_summaries") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) if a.len() <= MAX_FINDINGS => a
            .iter()
            .map(|f| {
                let text = |k: &str| f.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
                let time = match f.get("time") {
                    None | Some(Value::Null) => None,
                    Some(t) => Some(
                        t.as_i64()
                            .and_then(json::time_ms)
                            .ok_or_else(|| shape("finding time"))?,
                    ),
                };
                Ok(RevisionFinding {
                    report: text("report")
                        .ok_or_else(|| shape("finding report"))?
                        .into(),
                    ok: f
                        .get("ok")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| shape("finding ok"))?,
                    message: text("message")
                        .ok_or_else(|| shape("finding message"))?
                        .into(),
                    time,
                })
            })
            .collect::<Result<_>>()?,
        _ => return Err(shape("findings")),
    };
    let note = match (summary.get("is_stub"), summary.get("value")) {
        (Some(Value::Bool(true)), Some(Value::String(s))) if !s.is_empty() => Some(s.clone()),
        (Some(Value::Bool(true)), _) => return Err(shape("summary stub")),
        _ => None,
    };
    if findings.is_empty() && note.is_none() {
        return Err(shape("summary has neither findings nor a note"));
    }
    Ok(DeploymentRevision {
        id: id.to_string(),
        hash: hash.to_string(),
        started_at,
        version: label,
        status,
        findings,
        note,
    })
}

fn check_bound(max_revisions: usize) -> Result<()> {
    if max_revisions == 0 {
        return Err(Error::invalid("max_revisions must be at least 1"));
    }
    Ok(())
}

impl DeploymentRevision {
    /// Decodes the revisions from an answer of `GET app/<id>` the caller already has, such
    /// as one read with [`Project::get`], so one answer can feed several decoders.
    /// [`Project::deployment_revisions_with`] is this after its own request, and the
    /// contract is the same: `max_revisions == 0` or an empty `app` fail with
    /// [`ErrorKind::InvalidInput`](crate::ErrorKind::InvalidInput), anything else wrong
    /// with [`ErrorKind::Decode`](crate::ErrorKind::Decode).
    pub fn list_from_envelope(
        env: &Envelope,
        app: &AppId,
        max_revisions: usize,
    ) -> Result<Vec<Self>> {
        check_bound(max_revisions)?;
        check_app(app)?;
        revisions_from(env, app, max_revisions)
    }
}

fn revisions_from(env: &Envelope, app: &AppId, max: usize) -> Result<Vec<DeploymentRevision>> {
    let reports = app_reports(env, app)?;
    let mut found = reports
        .iter()
        .filter(|r| r.get("name").and_then(Value::as_str) == Some("Deployments"));
    // Coroot omits the report when the application has no deployments.
    let Some(report) = found.next() else {
        return Ok(Vec::new());
    };
    if found.next().is_some() {
        return Err(shape("several Deployments reports"));
    }
    let widgets = match report.get("widgets") {
        Some(Value::Array(w)) if w.iter().all(Value::is_object) => w,
        _ => return Err(shape("widgets")),
    };
    let mut tables = widgets
        .iter()
        .filter_map(|w| w.get("table").filter(|t| !t.is_null()))
        .filter(|t| {
            t.get("header").and_then(Value::as_array).is_some_and(|h| {
                h.iter()
                    .map(Value::as_str)
                    .eq(HEADER.iter().map(|s| Some(*s)))
            })
        });
    let table = tables.next().ok_or_else(|| shape("no deployments table"))?;
    if tables.next().is_some() {
        return Err(shape("several deployments tables"));
    }
    let rows = match table.get("rows") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(r)) if r.iter().all(Value::is_object) => r,
        _ => return Err(shape("rows")),
    };
    if rows.len() > max {
        return Err(Error::decode(format!(
            "application response has {} deployments, over the limit of {max}",
            rows.len()
        )));
    }
    let out = rows.iter().map(revision).collect::<Result<Vec<_>>>()?;
    let mut ids = std::collections::HashSet::new();
    if !out.iter().all(|r| ids.insert(r.id.as_str())) {
        return Err(shape("duplicate revision id"));
    }
    Ok(out)
}

impl Project {
    /// The deployment revisions Coroot keeps for an application (its last 100), with
    /// [`DEFAULT_MAX_REVISIONS`] as the bound. See [`Project::deployment_revisions_with`].
    pub async fn deployment_revisions(&self, app: &AppId) -> Result<Vec<DeploymentRevision>> {
        self.deployment_revisions_with(app, DEFAULT_MAX_REVISIONS)
            .await
    }

    /// The deployment revisions of an application, from the Deployments report of
    /// `GET app/<id>`, in the order Coroot sends them (newest first). Works with any
    /// credentials. Empty when Coroot knows no deployment of the application.
    ///
    /// **Which deployments.** Coroot keeps the last 100 deployments of each application;
    /// that is the whole history available. The project's time window does not filter
    /// them: it only decides whether the application exists, and an application it does
    /// not know fails with [`ErrorKind::NotFound`](crate::ErrorKind::NotFound).
    ///
    /// **Identity.** A revision is identified by [`DeploymentRevision::id`], the report's
    /// row id (`<hash>:<start unix seconds>`). `overview/deployments`
    /// ([`Project::deployments`]) has no id and only an inferred start time; use it for the
    /// project-wide list and this call for one application.
    ///
    /// **What each revision carries.** Coroot compares a revision with the *previous
    /// deployment that has a metrics snapshot* and sends the outcome as text
    /// ([`RevisionFinding`]). It does not say which deployment that was, so this crate does
    /// not claim one: do not present a finding as a comparison with the revision listed
    /// next to it. Some findings are absolute, not comparisons at all (an objective breach,
    /// an OOM kill, a crash, a memory leak), and a revision with no earlier snapshot is
    /// compared with nothing. Read a finding as the server's verdict on the revision, never
    /// as a diff.
    ///
    /// **Not available through Coroot's REST API** (so not provided here): a revision's
    /// historical Kubernetes spec (the current spec is not a stand-in), per-revision metric
    /// snapshots or series, and a comparison of two revisions you choose.
    ///
    /// A row id that is not `<hash>:<seconds>`, duplicate ids, a Deployments report
    /// without its table, more than `max_revisions` rows, or a summary with neither
    /// findings nor a note fail with [`ErrorKind::Decode`](crate::ErrorKind::Decode);
    /// `max_revisions == 0` fails with [`ErrorKind::InvalidInput`](crate::ErrorKind::InvalidInput)
    /// before any request is sent.
    pub async fn deployment_revisions_with(
        &self,
        app: &AppId,
        max_revisions: usize,
    ) -> Result<Vec<DeploymentRevision>> {
        check_bound(max_revisions)?;
        check_app(app)?;
        let env = self
            .get(&format!("app/{}", encode_segment(app.as_str())), &[])
            .await?;
        DeploymentRevision::list_from_envelope(&env, app, max_revisions)
    }
}

/// The default bound of [`Project::deployment_revisions`]: the number of deployments
/// Coroot keeps per application.
pub const DEFAULT_MAX_REVISIONS: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Envelope;
    use serde_json::json;

    const APP: &str = "c1:shop:Deployment:api";

    fn cell(value: &str) -> Value {
        json!({"value": value, "status": null, "is_stub": false, "deployment_summaries": null})
    }

    fn row(id: &str, status: &str, summary: Value) -> Value {
        json!({"id": id, "cells": [
            {"value": format!("{}: example.test/acme/api:1", id.split(':').next().unwrap()),
             "status": status, "tags": ["age: 2h"]},
            cell("2h ago"),
            summary]})
    }

    fn stub(text: &str) -> Value {
        json!({"value": text, "is_stub": true, "deployment_summaries": null})
    }

    fn findings(f: Value) -> Value {
        json!({"value": "", "is_stub": false, "deployment_summaries": f})
    }

    fn env(rows: Value) -> Envelope {
        let table = json!({"header": ["Deployment", "Deployed", "Summary"], "rows": rows});
        Envelope {
            context: json!({}),
            data: json!({"app_map": {"application": {"id": APP}}, "reports": [
                {"name": "SLO", "status": "ok", "widgets": []},
                {"name": "Deployments", "status": "ok", "widgets": [{"table": table}]}]}),
        }
    }

    fn parse(rows: Value) -> Result<Vec<DeploymentRevision>> {
        revisions_from(&env(rows), &AppId::new(APP), 10)
    }

    #[test]
    fn decodes_ids_findings_and_notes_in_server_order() {
        let rows = json!([
            row("5d9c7f:1790003600", "unknown", stub("Collecting data...")),
            row(
                "7b8e21:1790000000",
                "critical",
                findings(json!([
                    {"report": "SLO", "ok": false, "message": "Availability: 98% (objective: 99%)", "time": null},
                    {"report": "Instances", "ok": true, "message": "Restarts: 0", "time": 1790000100000_i64}]))
            ),
            row("a1b2c3:1789990000", "ok", stub("No notable changes")),
        ]);
        let revs = parse(rows).unwrap();
        let ids: Vec<_> = revs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "5d9c7f:1790003600",
                "7b8e21:1790000000",
                "a1b2c3:1789990000"
            ]
        );
        assert_eq!(revs[1].hash, "7b8e21");
        assert_eq!(revs[1].started_at.timestamp(), 1790000000);
        assert_eq!(revs[1].status, Status::Critical);
        assert_eq!(revs[1].findings.len(), 2);
        assert!(!revs[1].findings[0].ok);
        assert!(revs[1].findings[0].time.is_none());
        assert_eq!(revs[1].findings[1].time.unwrap().timestamp(), 1790000100);
        assert_eq!(revs[0].note.as_deref(), Some("Collecting data..."));
        assert_eq!(revs[0].status, Status::Unknown);
        assert_eq!(revs[2].note.as_deref(), Some("No notable changes"));
        assert!(revs[2].findings.is_empty());
        let back: Vec<DeploymentRevision> =
            serde_json::from_value(serde_json::to_value(&revs).unwrap()).unwrap();
        assert_eq!(back, revs);
    }

    #[test]
    fn no_report_or_no_rows_is_an_empty_list() {
        let none = Envelope {
            context: json!({}),
            data: json!({"app_map": {"application": {"id": APP}}, "reports": []}),
        };
        assert!(
            revisions_from(&none, &AppId::new(APP), 10)
                .unwrap()
                .is_empty()
        );
        assert!(parse(Value::Null).unwrap().is_empty());
        assert!(parse(json!([])).unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_rows_and_tables() {
        let ok = || row("7b8e21:1790000000", "ok", stub("No notable changes"));
        let with = |f: &dyn Fn(&mut Value)| {
            let mut r = ok();
            f(&mut r);
            json!([r])
        };
        let bad = [
            with(&|r| r["id"] = json!("")),
            with(&|r| r["id"] = json!("nocolon")),
            with(&|r| r["id"] = json!("abc:soon")),
            with(&|r| r["id"] = json!(":1790000000")),
            with(&|r| r["id"] = json!("abc:0")),
            with(&|r| r["cells"] = json!([])),
            with(&|r| r["cells"][0]["value"] = json!("")),
            with(&|r| r["cells"][0]["status"] = json!(3)),
            with(&|r| r["cells"][2] = cell("")),
            with(&|r| r["cells"][2] = stub("")),
            with(&|r| {
                r["cells"][2] = findings(json!([{"report": "SLO", "ok": "no", "message": "x"}]))
            }),
            with(&|r| {
                r["cells"][2] = findings(json!([{"report": "", "ok": true, "message": "x"}]))
            }),
            with(&|r| r["cells"][2] = findings(json!({}))),
            json!([ok(), ok()]),
            json!([null]),
            json!({}),
        ];
        for rows in bad {
            assert_eq!(
                parse(rows.clone()).unwrap_err().kind(),
                crate::ErrorKind::Decode,
                "{rows}"
            );
        }
        // A Deployments report whose table is missing or has another header is not "no rows".
        let mut e = env(json!([]));
        e.data["reports"][1]["widgets"] = json!([]);
        assert!(revisions_from(&e, &AppId::new(APP), 10).is_err());
        let mut e = env(json!([]));
        e.data["reports"][1]["widgets"][0]["table"]["header"] = json!(["A", "B", "C"]);
        assert!(revisions_from(&e, &AppId::new(APP), 10).is_err());
    }

    #[test]
    fn bounds_the_number_of_revisions() {
        let rows: Vec<_> = (0..3)
            .map(|i| {
                row(
                    &format!("h{i}:{}", 1790000000 + i),
                    "ok",
                    stub("No notable changes"),
                )
            })
            .collect();
        let e = env(json!(rows));
        assert!(revisions_from(&e, &AppId::new(APP), 3).is_ok());
        assert!(revisions_from(&e, &AppId::new(APP), 2).is_err());
    }
}
