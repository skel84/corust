//! traces, trace (via Coroot's MCP tools).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use coroot_rs::{Span, TraceQuery};
use serde_json::json;

use super::Ctx;
use crate::cli::{TraceArgs, TracesArgs};
use crate::error::{Kind, err};
use crate::output::{self, Table};
use crate::timeparse::parse_duration_ms;

pub async fn summary(ctx: &Ctx, args: TracesArgs) -> Result<()> {
    let project = ctx.project().await?;
    let q = TraceQuery {
        service: args.service.clone(),
        span: args.span.clone(),
    };
    if args.errors {
        let errors = project.trace_errors(&q).await?;
        return ctx.out.emit(&errors, || {
            let mut t = Table::new(&["count", "service", "span", "error", "sample trace"]);
            for i in &errors.items {
                t.row(vec![
                    output::number(i.count),
                    i.service_name.clone(),
                    i.span_name.clone(),
                    i.sample_error.replace('\n', " "),
                    i.sample_trace_id.clone(),
                ]);
            }
            if t.is_empty() {
                println!("no errors in the selected time range");
            } else {
                t.print();
            }
        });
    }
    if let Some(slow) = &args.slow {
        let ms = parse_duration_ms(slow).map_err(|e| err(Kind::Usage, e.to_string()))?;
        let v = project
            .trace_outliers(&q, Duration::from_millis(ms.max(0) as u64), None)
            .await?;
        let v = if v.is_null() {
            json!({"total": 0, "returned": 0, "items": []})
        } else {
            v
        };
        return ctx.out.emit(&v, || {
            let _ = output::print_json(&v);
        });
    }
    let summary = project.traces_summary(&q).await?;
    ctx.out.emit(&summary, || {
        let mut t = Table::new(&["service", "span", "requests", "errors", "p50", "p95", "p99"]);
        for i in &summary.endpoints.items {
            let q = &i.duration_quantiles;
            t.row(vec![
                i.service_name.clone(),
                i.span_name.clone(),
                output::number(i.total),
                if i.failed > 0.0 {
                    output::red(&format!("{:.1}%", i.error_percent()))
                } else {
                    String::new()
                },
                q.first().map(|x| output::latency(*x)).unwrap_or_default(),
                q.get(1).map(|x| output::latency(*x)).unwrap_or_default(),
                q.get(2).map(|x| output::latency(*x)).unwrap_or_default(),
            ]);
        }
        if t.is_empty() {
            println!(
                "no traces in the selected time range (is OpenTelemetry tracing or eBPF tracing enabled?)"
            );
        } else {
            t.print();
        }
    })
}

pub async fn trace(ctx: &Ctx, args: TraceArgs) -> Result<()> {
    let spans = ctx.project().await?.trace(&args.trace_id).await?;
    ctx.out.emit(&spans, || render_tree(&spans.items))
}

/// Prints spans as an indented tree with a timeline bar.
fn render_tree(spans: &[Span]) {
    let start = spans.iter().map(|s| s.timestamp).min().unwrap_or_default();
    let end = spans
        .iter()
        .map(|s| s.timestamp as f64 + s.duration)
        .fold(start as f64, f64::max);
    let total = (end - start as f64).max(1.0);
    let ids: std::collections::HashSet<&str> = spans.iter().map(|x| x.id.as_str()).collect();
    let mut children: HashMap<&str, Vec<&Span>> = HashMap::new();
    let mut roots = Vec::new();
    for sp in spans {
        let parent = sp.parent_id.as_str();
        if parent.is_empty() || !ids.contains(parent) {
            roots.push(sp);
        } else {
            children.entry(parent).or_default().push(sp);
        }
    }
    const BAR: usize = 30;
    let mut t = Table::new(&["span", "service", "duration", "timeline", "status"]);
    fn walk<'a>(
        sp: &'a Span,
        depth: usize,
        children: &HashMap<&str, Vec<&'a Span>>,
        t: &mut Table,
        start: i64,
        total: f64,
    ) {
        let offset = (sp.timestamp - start) as f64;
        let from = ((offset / total) * BAR as f64).floor() as usize;
        let width = (((sp.duration / total) * BAR as f64).ceil() as usize).max(1);
        let bar = format!(
            "{}{}",
            " ".repeat(from.min(BAR)),
            "█".repeat(width.min(BAR - from.min(BAR - 1)))
        );
        let failed = sp.status.error;
        let message = sp.status.message.as_str();
        t.row(vec![
            format!("{}{}", "  ".repeat(depth), sp.name),
            sp.service.clone(),
            output::latency(sp.duration / 1000.0),
            if failed {
                output::red(&bar)
            } else {
                output::cyan(&bar)
            },
            if failed {
                output::red(if message.is_empty() { "error" } else { message })
            } else {
                String::new()
            },
        ]);
        let mut kids = children.get(sp.id.as_str()).cloned().unwrap_or_default();
        kids.sort_by_key(|k| k.timestamp);
        for k in kids {
            walk(k, depth + 1, children, t, start, total);
        }
    }
    roots.sort_by_key(|k| k.timestamp);
    for r in roots {
        walk(r, 0, &children, &mut t, start, total);
    }
    t.print();
}
