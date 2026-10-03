//! query, metrics, mcp.

use std::time::Duration;

use anyhow::Result;
use coroot_rs::{MetricsQuery, ToolOutput};
use serde_json::{Map, Value};

use super::Ctx;
use crate::cli::{McpCmd, MetricsArgs, QueryArgs};
use crate::error::{Kind, err};
use crate::output::{self, Table};

pub async fn query(ctx: &Ctx, args: QueryArgs) -> Result<()> {
    let q = MetricsQuery {
        step: args.step.map(Duration::from_secs),
        limit: args.limit,
        ..MetricsQuery::new(&args.query)
    };
    let v = ctx.project().await?.query_metrics(&q).await?;
    ctx.out.emit(&v, || {
        let mut t = Table::new(&["series", "last", "min", "max", "trend"]);
        for series in &v.series {
            let labels: Vec<String> = series
                .labels
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let fmt = |x: Option<f64>| x.map(output::number).unwrap_or_default();
            t.row(vec![
                format!("{{{}}}", labels.join(", ")),
                fmt(series.last()),
                fmt(series.present().reduce(f64::min)),
                fmt(series.present().reduce(f64::max)),
                output::sparkline(&output::downsample(&series.values, 30)),
            ]);
        }
        if t.is_empty() {
            println!("no data");
        } else {
            t.print();
        }
        if v.truncated {
            eprintln!(
                "{}",
                output::dim(&format!(
                    "showing {} of {} series (use --limit)",
                    v.series_returned, v.series_total
                ))
            );
        }
    })
}

pub async fn names(ctx: &Ctx, args: MetricsArgs) -> Result<()> {
    // A plain word is a substring search; anything with regex syntax is passed through.
    let pattern = match args.pattern.as_deref() {
        None | Some("") => ".+".to_string(),
        Some(p)
            if p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':') =>
        {
            format!(".*{p}.*")
        }
        Some(p) => p.to_string(),
    };
    let v = ctx
        .project()
        .await?
        .metric_names(&pattern, args.limit)
        .await?;
    ctx.out.emit(&v, || {
        for n in &v.names {
            println!("{n}");
        }
        if v.truncated {
            eprintln!(
                "{}",
                output::dim(&format!(
                    "showing {} of {} (use --limit)",
                    v.returned, v.total
                ))
            );
        }
    })
}

/// Parses KEY=VALUE pairs (values as JSON when possible) or a single JSON object.
fn tool_args(args: &[String]) -> Result<Value> {
    if let [one] = args
        && one.trim_start().starts_with('{')
    {
        return serde_json::from_str(one)
            .map_err(|e| err(Kind::Usage, format!("invalid JSON arguments: {e}")));
    }
    let mut m = Map::new();
    for a in args {
        let Some((k, v)) = a.split_once('=') else {
            return Err(err(
                Kind::Usage,
                format!("invalid argument '{a}': expected KEY=VALUE"),
            ));
        };
        let value =
            serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.to_string()));
        m.insert(k.to_string(), value);
    }
    Ok(Value::Object(m))
}

pub async fn mcp(ctx: &Ctx, cmd: McpCmd) -> Result<()> {
    match cmd {
        McpCmd::Tools => {
            let mut mcp = ctx.client.mcp().await?;
            let tools = mcp.list_tools().await;
            mcp.close().await;
            let v = Value::Array(tools?);
            ctx.out.emit_value(&v, || {
                let mut t = Table::new(&["tool", "arguments", "description"]);
                for tool in v.as_array().into_iter().flatten() {
                    let schema = tool.get("inputSchema").unwrap_or(&Value::Null);
                    let required: Vec<&str> = schema
                        .get("required")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let params: Vec<String> = schema
                        .get("properties")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flatten()
                        .map(|(k, _)| {
                            if required.contains(&k.as_str()) {
                                format!("{k}*")
                            } else {
                                k.clone()
                            }
                        })
                        .collect();
                    let text =
                        |key: &str| tool.get(key).and_then(Value::as_str).unwrap_or_default();
                    let desc = text("description").lines().next().unwrap_or_default();
                    t.row(vec![
                        text("name").to_string(),
                        params.join(" "),
                        desc.to_string(),
                    ]);
                }
                t.print();
            })
        }
        McpCmd::Call {
            tool,
            args,
            no_project,
        } => {
            let a = tool_args(&args)?;
            let mut mcp = if no_project || tool == "list_projects" || tool == "select_project" {
                ctx.client.mcp().await?
            } else {
                ctx.project().await?.mcp().await?
            };
            let res = mcp.call_tool(&tool, a).await;
            mcp.close().await;
            match res? {
                ToolOutput::Text(text) if !ctx.out.is_machine() => {
                    println!("{text}");
                    Ok(())
                }
                out => output::print_json(&out.into_value()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tool_args;

    #[test]
    fn args() {
        let v = tool_args(&[
            "limit=5".into(),
            "state=firing".into(),
            "severity=[\"error\"]".into(),
        ])
        .unwrap();
        assert_eq!(v["limit"], 5);
        assert_eq!(v["state"], "firing");
        assert_eq!(v["severity"][0], "error");
        let v = tool_args(&["{\"a\": 1}".into()]).unwrap();
        assert_eq!(v["a"], 1);
        assert!(tool_args(&["nope".into()]).is_err());
    }
}
