//! incidents, incident.

use anyhow::Result;
use coroot_rs::{IncidentQuery, IncidentState};

use super::Ctx;
use crate::cli::{IncidentArgs, IncidentsArgs};
use crate::output::{self, Table, kv, section};

pub async fn list(ctx: &Ctx, args: IncidentsArgs) -> Result<()> {
    let project = ctx.project().await?;
    let app = match &args.app {
        Some(a) => Some(project.resolve_app(a).await?),
        None => None,
    };
    let rows = project
        .incidents(&IncidentQuery {
            app,
            state: args.state.into(),
            limit: args.limit,
        })
        .await?;
    ctx.out.emit(&rows, || {
        let mut t = Table::new(&[
            "",
            "key",
            "application",
            "opened",
            "duration",
            "impact",
            "description",
            "rca",
        ]);
        for r in &rows {
            t.row(vec![
                if r.state == IncidentState::Open {
                    output::status(r.severity.as_str())
                } else {
                    output::dim("resolved")
                },
                r.key.clone(),
                r.app.short(),
                r.opened_at
                    .map(|o| output::ago(o.timestamp_millis()))
                    .unwrap_or_default(),
                output::duration(r.duration.as_secs() as i64),
                format!("{:.1}%", r.impact_percent),
                r.description.clone(),
                r.rca
                    .as_ref()
                    .map(|c| {
                        if c.summary.is_empty() {
                            c.status.clone()
                        } else {
                            c.summary.clone()
                        }
                    })
                    .unwrap_or_default(),
            ]);
        }
        if t.is_empty() {
            println!("no incidents");
        } else {
            t.print();
        }
    })
}

pub async fn show(ctx: &Ctx, args: IncidentArgs) -> Result<()> {
    let project = ctx.project().await?;
    let inc = project.incident(&args.key).await?;
    ctx.out.emit(&inc, || {
        let time = |t: Option<chrono::DateTime<chrono::Utc>>| {
            t.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .unwrap_or_default()
        };
        kv(&[
            ("Incident", inc.key.clone()),
            ("Application", inc.app.short()),
            ("Severity", output::status(inc.severity.as_str())),
            (
                "State",
                match inc.state {
                    IncidentState::Open => "open",
                    IncidentState::Resolved => "resolved",
                }
                .to_string(),
            ),
            ("Opened", time(inc.opened_at)),
            ("Resolved", time(inc.resolved_at)),
            ("Duration", output::duration(inc.duration.as_secs() as i64)),
            ("Impact", format!("{:.1}% of requests", inc.impact_percent)),
            ("Description", inc.description.clone()),
            ("Link", project.incident_url(&inc.key).to_string()),
        ]);
        if let Some(r) = &inc.rca {
            section(&format!("Root cause analysis ({})", r.status));
            for (title, text) in [
                ("Summary", &r.summary),
                ("Root cause", &r.root_cause),
                ("Immediate fixes", &r.immediate_fixes),
                ("Details", &r.detailed_analysis),
                ("Error", &r.error),
            ] {
                if !text.is_empty() {
                    println!("{}\n{}\n", output::bold(title), text.trim());
                }
            }
            if !r.propagation.is_empty() {
                println!("{}", output::bold("Propagation"));
                for a in &r.propagation {
                    println!(
                        "  {} {}: {}",
                        output::status_dot(a.status.as_str()),
                        a.app_id.short(),
                        a.issues.join("; ")
                    );
                }
            }
        }
    })
}
