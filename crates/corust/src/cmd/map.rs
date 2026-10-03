//! map: the service map as a graph of applications and their connections.

use std::collections::HashMap;

use anyhow::Result;
use coroot_rs::{AppId, ServiceMap};

use super::Ctx;
use crate::cli::MapArgs;
use crate::output::{self, Table};

pub async fn map(ctx: &Ctx, args: MapArgs) -> Result<()> {
    let mut g = ctx.project().await?.service_map().await?;
    if let Some(query) = &args.app {
        let id = g.find(query)?;
        g.focus(&id, args.depth, args.direction.into());
    }
    if args.problems {
        g.retain_problems();
    }
    ctx.out.emit(&g, || render(ctx, &g))
}

fn render(ctx: &Ctx, g: &ServiceMap) {
    let short: HashMap<&AppId, String> = g.nodes.iter().map(|n| (&n.id, n.id.short())).collect();
    let name = |id: &AppId| short.get(id).cloned().unwrap_or_else(|| id.short());
    if ctx.out.wide() {
        output::section("Applications");
        let mut t = Table::new(&["status", "application", "category", "problems"]);
        for n in &g.nodes {
            let problems: Vec<&str> = n
                .indicators
                .iter()
                .filter(|(_, st)| st.is_problem())
                .map(|(k, _)| k.as_str())
                .collect();
            t.row(vec![
                output::status(n.status.as_str()),
                n.id.short(),
                n.category.clone(),
                problems.join(", "),
            ]);
        }
        t.print();
        output::section("Connections");
    }
    let mut t = Table::new(&[
        "status",
        "client",
        "dependency",
        "rps",
        "latency",
        "traffic ↑/↓",
        "issue",
    ]);
    for e in &g.edges {
        let traffic = match (e.sent_bytes_per_second, e.received_bytes_per_second) {
            (Some(tx), Some(rx)) => format!("{}/s {}/s", output::bytes(tx), output::bytes(rx)),
            _ => String::new(),
        };
        t.row(vec![
            output::status(e.status.as_str()),
            name(&e.from),
            name(&e.to),
            e.rps.map(output::number).unwrap_or_default(),
            e.latency_seconds.map(output::latency).unwrap_or_default(),
            traffic,
            output::red(&e.issue),
        ]);
    }
    if t.is_empty() {
        println!("no connections");
    } else {
        t.print();
    }
}
