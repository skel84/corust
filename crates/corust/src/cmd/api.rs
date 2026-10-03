//! api, open.

use std::io::{Read, Write};

use anyhow::{Context as _, Result};
use coroot_rs::Method;
use coroot_rs::util::{encode_segment, is_html};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};

use super::Ctx;
use crate::cli::{ApiArgs, OpenArgs};
use crate::error::{Kind, err};
use crate::output;

pub async fn api(ctx: &Ctx, args: ApiArgs) -> Result<()> {
    let method = Method::from_bytes(args.method.to_uppercase().as_bytes())
        .map_err(|_| err(Kind::Usage, "invalid HTTP method"))?;
    let mut path = args.path.trim_start_matches('/').to_string();
    if path.contains("{project}") {
        path = path.replace("{project}", &encode_segment(ctx.project().await?.id()));
    }
    let mut query: Vec<(String, String)> = Vec::new();
    for q in &args.query {
        let (k, v) = q.split_once('=').ok_or_else(|| {
            err(
                Kind::Usage,
                format!("invalid query parameter '{q}': expected KEY=VALUE"),
            )
        })?;
        query.push((k.to_string(), v.to_string()));
    }
    // Project views read the global time range unless the caller set it explicitly.
    for (k, v) in ctx.range.query() {
        if !query.iter().any(|(qk, _)| qk == k) {
            query.push((k.to_string(), v));
        }
    }
    let mut rb = ctx
        .client
        .request(method, &path)?
        .header(ACCEPT, "application/json")
        .query(&query);
    if let Some(data) = &args.data {
        let body = match data.strip_prefix('@') {
            Some("-") => {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            }
            Some(file) => {
                std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?
            }
            None => data.clone(),
        };
        rb = rb.header(CONTENT_TYPE, "application/json").body(body);
    }
    let resp = ctx.client.execute(rb).await?;
    let text = resp
        .text()
        .await
        .map_err(|e| err(Kind::Network, format!("cannot read the response: {e}")))?;
    if path.starts_with("api/") && is_html(&text) {
        return Err(err(
            Kind::NotFound,
            format!("unknown API endpoint: /{path}"),
        ));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => output::print_json(&v),
        Err(_) => {
            std::io::stdout().write_all(text.as_bytes())?;
            Ok(())
        }
    }
}

const VIEWS: [&str; 12] = [
    "applications",
    "incidents",
    "alerts",
    "map",
    "nodes",
    "kubernetes",
    "traces",
    "logs",
    "costs",
    "anomalies",
    "risks",
    "dashboards",
];

pub async fn open(ctx: &Ctx, args: OpenArgs) -> Result<()> {
    let target = args.target.as_deref().unwrap_or("applications");
    let project = ctx.project().await?;
    let url = match target.split_once('/') {
        Some(("incident", key)) => project.incident_url(key),
        Some(("alert", id)) => project.alert_url(id),
        Some(("node", n)) => project.node_url(&project.resolve_node(n).await?),
        Some(("app", a)) => project.app_url(&project.resolve_app(a).await?),
        _ if VIEWS.contains(&target) => project.view_url(target),
        _ if target == "deployments" => project.view_url("applications"),
        _ => project.app_url(&project.resolve_app(target).await?),
    };
    if args.browser {
        open_browser(url.as_str())?;
    }
    ctx.out
        .emit_value(&json!({"url": url.as_str()}), || println!("{url}"))
}

fn open_browser(url: &str) -> Result<()> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(url)
        .status()
        .with_context(|| format!("cannot run {cmd}"))?;
    Ok(())
}
