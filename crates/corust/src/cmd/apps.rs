//! apps, app.

use anyhow::Result;
use coroot_rs::{AppHealth, Application, Credentials, Status};

use super::Ctx;
use crate::cli::{AppArgs, AppsArgs};
use crate::output::{self, Format, Table, kv, section};

pub async fn list(ctx: &Ctx, args: AppsArgs) -> Result<()> {
    let apps = ctx.project().await?.applications().await?;
    let min = if args.problems {
        Status::Warning
    } else {
        args.status.map(|s| s.status()).unwrap_or(Status::Unknown)
    };
    let filter = args.filter.as_deref().map(str::to_lowercase);
    let rows: Vec<Application> = apps
        .into_iter()
        .filter(|a| {
            filter
                .as_ref()
                .is_none_or(|f| a.id.as_str().to_lowercase().contains(f))
                && (args.category.is_empty()
                    || args
                        .category
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&a.category)))
                && args
                    .namespace
                    .as_ref()
                    .is_none_or(|ns| a.id.namespace() == Some(ns.as_str()))
                && a.status >= min
        })
        .collect();
    ctx.out.emit(&rows, || {
        let wide = ctx.out.wide();
        let mut headers = vec![
            "",
            "application",
            "type",
            "category",
            "errors",
            "latency",
            "instances",
            "restarts",
            "cpu",
            "memory",
            "logs",
        ];
        if wide {
            headers.extend(["upstreams", "disk_io", "disk_usage", "network", "dns", "id"]);
        }
        let mut t = Table::new(&headers);
        let cell = |r: &Application, k: &str| {
            r.signals
                .get(k)
                .map(|s| output::by_status(s.status.as_str(), &s.value))
                .unwrap_or_default()
        };
        for r in &rows {
            let mut row = vec![
                output::status_dot(r.status.as_str()),
                r.id.short(),
                r.app_type.clone(),
                r.category.clone(),
                cell(r, "errors"),
                cell(r, "latency"),
                cell(r, "instances"),
                cell(r, "restarts"),
                cell(r, "cpu"),
                cell(r, "memory"),
                cell(r, "logs"),
            ];
            if wide {
                row.extend([
                    cell(r, "upstreams"),
                    cell(r, "disk_io_load"),
                    cell(r, "disk_usage"),
                    cell(r, "network"),
                    cell(r, "dns"),
                    r.id.to_string(),
                ]);
            }
            t.row(row);
        }
        if t.is_empty() {
            println!("no matching applications");
        } else {
            t.print();
        }
    })
}

pub async fn show(ctx: &Ctx, args: AppArgs) -> Result<()> {
    let project = ctx.project().await?;
    let id = project.resolve_app(&args.app).await?;
    // Raw output shows Coroot's REST payload, which has more than the MCP summary.
    let rest = args.rest
        || ctx.out.format == Format::Raw
        || !matches!(ctx.client.credentials(), Credentials::ApiKey(_));
    let health = if rest {
        project.app_health_rest(&id).await?
    } else {
        project.app_health(&id).await?
    };
    ctx.out.emit(&health, || {
        render_health(project.app_url(&id).as_str(), &health)
    })
}

fn render_health(link: &str, h: &AppHealth) {
    kv(&[
        ("Application", h.id.short()),
        ("Id", h.id.to_string()),
        ("Status", output::status(h.status.as_str())),
        ("Link", link.to_string()),
    ]);

    if !h.vitals.is_empty() {
        section("Vitals");
        let mut t = Table::new(&["metric", "last", "avg", "max", "trend"]);
        for m in &h.vitals {
            let fmt = |x: Option<f64>| match x {
                None => String::new(),
                Some(x) if m.name.ends_with("_seconds") => output::latency(x),
                Some(x) if m.name.ends_with("_bytes") => output::bytes(x),
                Some(x) => output::number(x),
            };
            t.row(vec![
                m.name.clone(),
                fmt(m.last),
                fmt(m.avg),
                fmt(m.max),
                output::sparkline(&m.sparkline),
            ]);
        }
        t.print();
    }

    section("Inspections");
    let mut t = Table::new(&["", "inspection", "issues"]);
    for r in &h.reports {
        let issues: Vec<String> = r
            .issues
            .iter()
            .map(|i| {
                let text = if i.message.is_empty() {
                    i.title.clone()
                } else {
                    format!("{}: {}", i.title, i.message)
                };
                output::by_status(i.status.as_str(), &text)
            })
            .collect();
        t.row(vec![
            output::status_dot(r.status.as_str()),
            r.name.clone(),
            issues.join("; "),
        ]);
    }
    t.print();

    let patterns: Vec<_> = h.reports.iter().flat_map(|r| &r.log_patterns).collect();
    if !patterns.is_empty() {
        section("Log patterns");
        let mut t = Table::new(&["severity", "messages", "hash", "sample"]);
        for p in patterns {
            t.row(vec![
                output::severity(&p.severity),
                p.messages.to_string(),
                p.hash.clone(),
                p.sample.replace('\n', " "),
            ]);
        }
        t.print();
    }

    if !h.dependencies.is_empty() {
        section("Dependencies");
        let mut t = Table::new(&["", "application", "rps", "latency", "connectivity"]);
        for d in &h.dependencies {
            let mut conn = d.connectivity.to_string();
            if !d.connectivity_message.is_empty() {
                conn = format!("{conn}: {}", d.connectivity_message);
            }
            t.row(vec![
                output::status_dot(d.status.max(d.connectivity).as_str()),
                d.id.short(),
                d.rps.map(output::number).unwrap_or_default(),
                d.latency_seconds
                    .as_ref()
                    .and_then(|l| l.p95.or(l.avg))
                    .map(output::latency)
                    .unwrap_or_default(),
                output::by_status(d.connectivity.as_str(), &conn),
            ]);
        }
        t.print();
    }

    if !h.clients.is_empty() {
        section("Clients");
        let mut t = Table::new(&["", "application", "rps", "latency"]);
        for c in &h.clients {
            t.row(vec![
                output::status_dot(c.status.as_str()),
                c.id.short(),
                c.rps.map(output::number).unwrap_or_default(),
                c.latency_seconds.map(output::latency).unwrap_or_default(),
            ]);
        }
        t.print();
    }
}
