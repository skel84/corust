//! projects, status, deployments, risks.

use std::collections::BTreeMap;

use anyhow::Result;
use coroot_rs::Component;
use serde::Serialize;

use super::Ctx;
use crate::output::{self, Table, kv, section};

#[derive(Serialize)]
struct ProjectRow {
    id: String,
    name: String,
    current: bool,
}

pub async fn projects(ctx: &Ctx) -> Result<()> {
    let projects = ctx.client.projects().await?;
    let current = ctx.project().await.ok().map(|p| p.id().to_string());
    let rows: Vec<ProjectRow> = projects
        .iter()
        .map(|p| ProjectRow {
            id: p.id.clone(),
            name: p.name.clone(),
            current: current.as_deref() == Some(&p.id),
        })
        .collect();
    ctx.out.emit(&rows, || {
        let mut t = Table::new(&["", "name", "id"]);
        for r in &rows {
            t.row(vec![
                if r.current { "*".into() } else { String::new() },
                r.name.clone(),
                r.id.clone(),
            ]);
        }
        t.print();
    })
}

pub async fn status(ctx: &Ctx) -> Result<()> {
    let st = ctx.project().await?.status().await?;
    ctx.out.emit(&st, || {
        let comp = |c: &Option<Component>| match c {
            Some(c) if c.message.is_empty() => output::status(c.status.as_str()),
            Some(c) => format!("{} {}", output::status(c.status.as_str()), c.message),
            None => output::dim("not configured"),
        };
        let fmt_counts = |m: &BTreeMap<String, u64>| {
            let parts: Vec<String> = m
                .iter()
                .filter(|(_, v)| **v > 0)
                .map(|(k, v)| format!("{v} {k}"))
                .collect();
            if parts.is_empty() {
                output::green("none")
            } else {
                parts.join(", ")
            }
        };
        kv(&[
            ("Project", st.project_id.clone()),
            (
                "Status",
                if st.error.is_empty() {
                    output::status(st.status.as_str())
                } else {
                    format!("{} {}", output::status(st.status.as_str()), st.error)
                },
            ),
            ("Prometheus", comp(&st.prometheus)),
            (
                "Node agent",
                st.node_agent
                    .as_ref()
                    .map(|n| format!("{} ({} nodes)", output::status(n.status.as_str()), n.nodes))
                    .unwrap_or_default(),
            ),
            ("Kube metrics", comp(&st.kube_state_metrics)),
            ("Applications", st.applications.to_string()),
            ("Nodes", st.nodes.to_string()),
            ("Alerts", fmt_counts(&st.alerts)),
            ("Incidents", fmt_counts(&st.incidents)),
        ]);
    })
}

pub async fn deployments(ctx: &Ctx) -> Result<()> {
    let rows = ctx.project().await?.deployments().await?;
    ctx.out.emit(&rows, || {
        let mut t = Table::new(&["application", "version", "deployed", "summary"]);
        for r in &rows {
            t.row(vec![
                r.app.short(),
                r.version.clone(),
                r.started_at
                    .map(|t| output::ago(t.timestamp_millis()))
                    .unwrap_or_default(),
                r.summary
                    .iter()
                    .map(|n| format!("{} {}", n.status, n.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            ]);
        }
        if t.is_empty() {
            println!("no deployments in the selected time range");
        } else {
            t.print();
        }
    })
}

pub async fn risks(ctx: &Ctx) -> Result<()> {
    let rows = ctx.project().await?.risks().await?;
    ctx.out.emit(&rows, || {
        section("Risks");
        let mut t = Table::new(&["severity", "application", "category", "risk"]);
        for r in &rows {
            t.row(vec![
                output::status(r.severity.as_str()),
                r.app.short(),
                r.category.clone(),
                r.description.clone(),
            ]);
        }
        if t.is_empty() {
            println!("no risks detected");
        } else {
            t.print();
        }
    })
}
