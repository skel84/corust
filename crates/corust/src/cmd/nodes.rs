//! nodes, node.

use anyhow::Result;
use coroot_rs::Node;

use super::Ctx;
use crate::cli::{NodeArgs, NodesArgs};
use crate::output::{self, Table, kv, section};

pub async fn list(ctx: &Ctx, args: NodesArgs) -> Result<()> {
    let filter = args.filter.as_deref().map(str::to_lowercase);
    let rows: Vec<Node> = ctx
        .project()
        .await?
        .nodes()
        .await?
        .into_iter()
        .filter(|n| {
            filter
                .as_ref()
                .is_none_or(|f| n.name.to_lowercase().contains(f))
        })
        .collect();
    ctx.out.emit(&rows, || {
        let wide = ctx.out.wide();
        let mut headers = vec![
            "", "name", "cluster", "status", "cpu", "memory", "uptime", "provider", "type", "zone",
        ];
        if wide {
            headers.extend(["ips", "os", "kernel"]);
        }
        let mut t = Table::new(&headers);
        let pct = |p: Option<f64>| p.map(output::percent).unwrap_or_default();
        for r in &rows {
            let mut row = vec![
                output::status_dot(r.status.as_str()),
                r.name.clone(),
                r.cluster.clone(),
                if r.status_message.is_empty() {
                    r.status.to_string()
                } else {
                    r.status_message.clone()
                },
                pct(r.cpu_percent),
                pct(r.memory_percent),
                output::duration(r.uptime.as_secs() as i64),
                r.cloud_provider.clone(),
                r.instance_type.clone(),
                r.availability_zone.clone(),
            ];
            if wide {
                row.extend([r.ips.join(","), r.os.clone(), r.kernel.clone()]);
            }
            t.row(row);
        }
        if t.is_empty() {
            println!("no nodes found");
        } else {
            t.print();
        }
    })
}

pub async fn show(ctx: &Ctx, args: NodeArgs) -> Result<()> {
    let project = ctx.project().await?;
    let id = project.resolve_node(&args.node).await?;
    let node = project.node(&id).await?;
    ctx.out.emit(&node, || {
        kv(&[
            ("Node", id.to_string()),
            ("Status", output::status(node.status.as_str())),
            ("Link", project.node_url(&id).to_string()),
        ]);
        for r in &node.reports {
            section(&format!(
                "{} {}",
                output::status_dot(r.status.as_str()),
                r.name
            ));
            let mut t = Table::new(&["", "check", "message"]);
            for c in &r.checks {
                t.row(vec![
                    output::status_dot(c.status.as_str()),
                    c.title.clone(),
                    c.message.clone(),
                ]);
            }
            if !t.is_empty() {
                t.print();
            }
            for (title, values) in &r.latest {
                let vals: Vec<String> = values
                    .iter()
                    .map(|(k, v)| format!("{k}={}", output::number(*v)))
                    .collect();
                println!(
                    "  {} {}",
                    output::dim(&format!("{title}:")),
                    vals.join("  ")
                );
            }
        }
    })
}
