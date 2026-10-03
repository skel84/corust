//! logs.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use coroot_rs::{LogEntry, LogFilter, LogQuery};

use super::Ctx;
use crate::cli::LogsArgs;
use crate::error::{Kind, err};
use crate::output::{self, Format, Table};

const FOLLOW_PAGE: usize = 10_000;

fn query(args: &LogsArgs) -> Result<LogQuery> {
    let mut q = LogQuery::new().limit(args.limit);
    if let Some(src) = args.source {
        q = q.source(src.into());
    }
    for sev in &args.severity {
        for s in sev.split(',').filter(|s| !s.is_empty()) {
            q = q.severity(s);
        }
    }
    for text in &args.search {
        q = q.contains(text);
    }
    for text in &args.exclude {
        q = q.excludes(text);
    }
    if let Some(t) = &args.trace_id {
        q = q.trace_id(t);
    }
    for expr in &args.filters {
        q = q.filter(LogFilter::parse(expr)?);
    }
    Ok(q)
}

fn print_line(e: &LogEntry, project_wide: bool) {
    let sev = format!("{:<7}", e.severity.to_uppercase());
    let app = if project_wide {
        format!("{} ", output::cyan(&e.app_id.short()))
    } else {
        String::new()
    };
    println!(
        "{} {} {}{}",
        output::dim(&output::timestamp_precise(e.timestamp.timestamp_millis())),
        output::severity_colored(&e.severity, &sev),
        app,
        e.message.trim_end()
    );
}

pub async fn run(ctx: &Ctx, args: LogsArgs) -> Result<()> {
    let project = ctx.project().await?;
    let mut q = query(&args)?;
    if let Some(a) = &args.app {
        q = q.app(project.resolve_app(a).await?);
    }
    let project_wide = q.app.is_none();

    if args.patterns {
        if project_wide {
            return Err(err(Kind::Usage, "--patterns needs an application"));
        }
        let rows = project.log_patterns(&q).await?;
        return ctx.out.emit(&rows, || {
            let mut t = Table::new(&["severity", "count", "hash", "sample"]);
            for p in &rows {
                t.row(vec![
                    output::severity(&p.severity),
                    p.count.to_string(),
                    p.hash.clone(),
                    p.sample.replace('\n', " "),
                ]);
            }
            t.print();
        });
    }

    let page = project.logs(&q).await?;
    if !args.follow {
        return ctx.out.emit(&page.entries, || {
            for e in &page.entries {
                print_line(e, project_wide);
            }
        });
    }

    // Follow: stream entries as they arrive. JSON formats become one object per line.
    let emit = |entries: &[LogEntry]| -> Result<()> {
        for e in entries {
            match ctx.out.format {
                Format::Json | Format::Jsonl | Format::Raw => {
                    println!("{}", serde_json::to_string(e)?)
                }
                _ => print_line(e, project_wide),
            }
        }
        std::io::stdout().flush()?;
        Ok(())
    };
    emit(&page.entries)?;
    // Each poll must return everything since the previous one.
    q.limit = FOLLOW_PAGE;
    q.since = page.cursor;
    loop {
        tokio::time::sleep(Duration::from_secs(args.interval.max(1))).await;
        let page = project.logs(&q).await?;
        if page.entries.len() >= FOLLOW_PAGE {
            eprintln!(
                "warning: more than {FOLLOW_PAGE} new entries since the last poll, some were skipped (lower --interval)"
            );
        }
        emit(&page.entries)?;
        q.since = page.cursor;
    }
}
