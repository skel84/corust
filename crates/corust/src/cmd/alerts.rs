//! alerts, alert, rules.

use std::io::Read;

use anyhow::{Context as _, Result};
use coroot_rs::{AlertAction, AlertQuery, AlertState};
use serde_json::{Value, json};

use super::Ctx;
use crate::cli::{AlertArgs, AlertsArgs, AlertsCmd, RulesArgs, RulesCmd};
use crate::error::{Kind, err};
use crate::output::{self, Table, kv, section};

fn state_name(s: AlertState) -> &'static str {
    match s {
        AlertState::Firing => "firing",
        AlertState::Suppressed => "suppressed",
        AlertState::Resolved => "resolved",
    }
}

pub async fn alerts(ctx: &Ctx, args: AlertsArgs) -> Result<()> {
    if let Some(action) = args.action {
        return act(ctx, action).await;
    }
    let project = ctx.project().await?;
    let app = match &args.app {
        Some(a) => Some(project.resolve_app(a).await?),
        None => None,
    };
    let rows = project
        .alerts(&AlertQuery {
            app,
            state: args.state.into(),
            search: args.search.clone(),
            limit: args.limit,
        })
        .await?;
    ctx.out.emit(&rows, || {
        let mut t = Table::new(&[
            "",
            "id",
            "application",
            "rule",
            "opened",
            "duration",
            "summary",
        ]);
        for r in &rows {
            t.row(vec![
                if r.state == AlertState::Firing {
                    output::status(r.severity.as_str())
                } else {
                    output::dim(state_name(r.state))
                },
                r.id.clone(),
                r.app.short(),
                r.rule_name.clone(),
                r.opened_at
                    .map(|o| output::ago(o.timestamp_millis()))
                    .unwrap_or_default(),
                output::duration(r.duration.as_secs() as i64),
                r.summary.clone(),
            ]);
        }
        if t.is_empty() {
            println!(
                "no {} alerts",
                match args.state {
                    crate::cli::AlertState::Firing => "firing",
                    crate::cli::AlertState::Resolved => "resolved",
                    crate::cli::AlertState::Any => "",
                }
            );
        } else {
            t.print();
        }
    })
}

async fn act(ctx: &Ctx, action: AlertsCmd) -> Result<()> {
    let (action, done, ids) = match action {
        AlertsCmd::Resolve { ids } => (AlertAction::Resolve, "resolved", ids),
        AlertsCmd::Suppress { ids } => (AlertAction::Suppress, "suppressed", ids),
        AlertsCmd::Reopen { ids } => (AlertAction::Reopen, "reopened", ids),
    };
    let ids = read_ids(ids)?;
    if ids.is_empty() {
        return Err(err(
            Kind::Usage,
            "no alert ids given (pass them as arguments or one per line on stdin with `-`)",
        ));
    }
    ctx.project().await?.update_alerts(action, &ids).await?;
    ctx.out
        .emit_value(&json!({"action": action.as_str(), "ids": ids}), || {
            println!("{done} {} alert(s): {}", ids.len(), ids.join(", "));
        })
}

/// Ids from arguments; a single "-" reads whitespace-separated ids from stdin.
fn read_ids(ids: Vec<String>) -> Result<Vec<String>> {
    if ids.len() == 1 && ids[0] == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s.split_whitespace().map(str::to_string).collect());
    }
    Ok(ids)
}

pub async fn show(ctx: &Ctx, args: AlertArgs) -> Result<()> {
    let project = ctx.project().await?;
    let a = project.alert(&args.id).await?;
    ctx.out.emit(&a, || {
        let time = |t: Option<chrono::DateTime<chrono::Utc>>| {
            t.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        };
        kv(&[
            ("Alert", a.id.clone()),
            ("Rule", format!("{} ({})", a.rule_name, a.rule_id)),
            ("Application", a.app.short()),
            ("Severity", output::status(a.severity.as_str())),
            ("State", state_name(a.state).to_string()),
            ("Summary", a.summary.clone()),
            ("Opened", time(a.opened_at).unwrap_or_default()),
            (
                "Resolved",
                time(a.resolved_at)
                    .map(|t| format!("{t} {}", a.resolved_by))
                    .unwrap_or_default(),
            ),
            ("Duration", output::duration(a.duration.as_secs() as i64)),
            ("Link", project.alert_url(&a.id).to_string()),
        ]);
        for d in &a.details {
            section(&d.name);
            println!("{}", d.value.trim());
        }
    })
}

fn read_json(file: &str) -> Result<Value> {
    let text = if file == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?
    };
    serde_json::from_str(&text)
        .map_err(|e| err(Kind::Usage, format!("invalid JSON in {file}: {e}")))
}

fn print_result(res: Option<Value>) -> Result<()> {
    match res {
        Some(v) => output::print_json(&v),
        None => Ok(()),
    }
}

pub async fn rules(ctx: &Ctx, args: RulesArgs) -> Result<()> {
    let project = ctx.project().await?;
    match args.action.unwrap_or(RulesCmd::List) {
        RulesCmd::List => {
            let rows = project.alert_rules().await?;
            ctx.out.emit(&rows, || {
                let mut t = Table::new(&[
                    "id", "name", "severity", "source", "enabled", "firing", "origin",
                ]);
                for r in &rows {
                    let kind = r.source.get("type").and_then(Value::as_str).unwrap_or("");
                    let source = match kind {
                        "check" => format!(
                            "check:{}",
                            r.source
                                .pointer("/check/check_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                        ),
                        other => other.to_string(),
                    };
                    t.row(vec![
                        r.id.clone(),
                        r.name.clone(),
                        output::status(r.severity.as_str()),
                        source,
                        if r.enabled {
                            output::green("yes")
                        } else {
                            output::dim("no")
                        },
                        if r.firing_alerts > 0 {
                            output::red(&r.firing_alerts.to_string())
                        } else {
                            String::new()
                        },
                        if r.readonly {
                            "config".into()
                        } else if r.builtin {
                            "builtin".into()
                        } else {
                            "custom".into()
                        },
                    ]);
                }
                t.print();
            })
        }
        RulesCmd::Get { id } => output::print_json(&project.alert_rule(&id).await?),
        RulesCmd::Export => {
            let yaml = project.export_alert_rules().await?;
            if ctx.out.is_machine() {
                output::print_json(&json!({"yaml": yaml}))
            } else {
                print!("{yaml}");
                Ok(())
            }
        }
        RulesCmd::Create { file } => {
            print_result(project.create_alert_rule(&read_json(&file)?).await?)
        }
        RulesCmd::Update { id, file } => {
            print_result(project.update_alert_rule(&id, &read_json(&file)?).await?)
        }
        RulesCmd::Enable { id } => set_enabled(ctx, &id, true).await,
        RulesCmd::Disable { id } => set_enabled(ctx, &id, false).await,
        RulesCmd::Delete { id } => {
            project.delete_alert_rule(&id).await?;
            ctx.out
                .emit_value(&json!({"deleted": id}), || println!("deleted rule {id}"))
        }
    }
}

async fn set_enabled(ctx: &Ctx, id: &str, enabled: bool) -> Result<()> {
    let now = ctx
        .project()
        .await?
        .set_alert_rule_enabled(id, enabled)
        .await?;
    ctx.out.emit_value(&json!({"id": id, "enabled": now}), || {
        println!("rule {id} {}", if now { "enabled" } else { "disabled" });
    })
}
