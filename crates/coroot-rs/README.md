# coroot-rs

An async Rust client for [Coroot](https://coroot.com), the open-source observability platform.

It reads what Coroot knows about a project and returns typed values:

- application health
- the service map
- incidents with root cause analysis
- alerts and alerting rules
- logs, traces and PromQL metrics
- nodes, deployments and risks

The [`corust`](../corust) CLI is built on it.

```toml
[dependencies]
# Not on crates.io yet: use a path while developing side by side...
coroot-rs = { path = "../corust/crates/coroot-rs" }
# ...or the git repository once it is published (Cargo finds the crate inside the workspace).
# coroot-rs = { git = "https://github.com/skel84/corust" }
```

It needs a Tokio runtime and Rust 1.88+ (edition 2024).

## Usage

```rust
use std::time::Duration;
use coroot_rs::{Client, Direction, ErrorKind, IncidentQuery, LogQuery, TimeRange};

async fn triage(key: String) -> coroot_rs::Result<()> {
    let client = Client::builder("https://coroot.example.com").api_key(key).build()?;
    let project = client
        .find_project("production")
        .await?
        .with_range(TimeRange::last(Duration::from_secs(3600)));

    // What is unhealthy, most severe first.
    for app in project.applications().await? {
        if app.status.is_problem() {
            println!("{} {} {:?}", app.status, app.id.short(), app.signals.keys());
        }
    }

    // Names resolve like in the UI: "api", "prod/api", "Deployment/api" or a full id.
    let db = match project.resolve_app("postgres").await {
        Ok(id) => id,
        Err(e) if e.kind() == ErrorKind::Ambiguous => {
            eprintln!("which one? {:?}", e.candidates());
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // Inspections, failed checks and connections of one application.
    let health = project.app_health(&db).await?;
    for (report, issue) in health.issues() {
        println!("{report}: {} {}", issue.title, issue.message);
    }

    // Everything that calls the database, two hops out.
    let mut map = project.service_map().await?;
    map.focus(&db, 2, Direction::Clients);

    let errors = project
        .logs(&LogQuery::new().app(&db).severity("error").limit(50))
        .await?;
    let incidents = project.incidents(&IncidentQuery::default()).await?;
    Ok(())
}
```

A runnable version is in [`examples/triage.rs`](examples/triage.rs): `cargo run -p coroot-rs --example triage -- <project>` with `COROOT_URL` and `COROOT_API_KEY` set.

## API overview

**`Client`** is cheap to clone, so one client can be shared between tasks.

- Build it with `Client::builder(url)`.
- Authenticate with `.api_key(..)`, `.session(..)`, or `.login(email, password).await`.
- Other builder options: `.timeout(..)`, `.connect_timeout(..)`, `.accept_invalid_certs(..)`, `.user_agent(..)`, `.http_client(..)`, `.max_response_bytes(..)` (described below) and `.on_payload(..)` (described below).
- Methods: `user()`, `projects()`, `project(id)`, `find_project(id_or_name)`, `mcp()`.
- For endpoints this crate does not wrap: `request()` with `execute()` and `read_body()`, `get_json()`, `send_json()`.

**`Project`** is the entry point for data. It carries a `TimeRange`, which `with_range` replaces. Its methods:

| Area | Methods |
|---|---|
| Overview | `status`, `deployments`, `risks` |
| Applications | `applications`, `app_health`, `app_health_rest`, `resolve_app`, `application_ids` |
| Service map | `service_map`, then `ServiceMap::focus`, `retain_problems`, `find`, `node` |
| Nodes | `nodes`, `node`, `resolve_node`, `node_ids` |
| Incidents | `incidents(&IncidentQuery)`, `incident(key)` with RCA and SLO burn rates |
| Alerts | `alerts(&AlertQuery)`, `alert`, `update_alerts(AlertAction, ids)` |
| Alerting rules | `alert_rules`, `alert_rule`, `create_alert_rule`, `update_alert_rule`, `delete_alert_rule`, `set_alert_rule_enabled`, `export_alert_rules` |
| Logs | `logs(&LogQuery) -> LogPage`, with a cursor for following; `log_patterns` |
| Traces | `traces_summary`, `trace_errors`, `trace_outliers`, `trace(id)` |
| Metrics | `query_metrics(&MetricsQuery)`, `metric_names(pattern, limit)` |
| UI links | `app_url`, `node_url`, `incident_url`, `alert_url`, `view_url`, `ui_url` |
| Escape hatches | `get`, `send`, `mcp` |

**Models**

- Every model implements `Serialize` and `Deserialize`. Its JSON form is stable, so a model can be cached, recorded as a test fixture, or handed to another program:
  - keys are snake_case;
  - times are RFC 3339;
  - durations are whole seconds in `*_seconds` fields;
  - missing lists are `[]`.
- `AppId` and `NodeId` are parsed ids. `AppId` gives `name()`, `namespace()`, `kind()`, `cluster_id()` and `short()`.
- `Status` is ordered: `Unknown < Ok < Info < Warning < Critical`. `is_problem()` means warning or worse.
- `Application::signals` retains every known signal field present in the response,
  including empty `Ok` and `Unknown` signals. An absent field has no map entry;
  a present null/malformed signal or missing/unrecognized status becomes `Unknown`.
  Missing/non-string values become empty strings without changing an explicitly
  reported status, and empty values are omitted from serialized JSON. Use
  `signal.status.is_problem()` to select problems; map membership alone is not a
  health verdict. This adds entries that earlier versions discarded.

**Errors**

Every `Error` has an `ErrorKind`:

| Kind | Meaning |
|---|---|
| `InvalidInput` | A bad argument: a malformed filter or PromQL, an empty id list, a result too large for MCP (narrow the query) |
| `Auth` | Missing or rejected credentials, an expired session, or an API key required (MCP) |
| `Forbidden` | The user's role does not allow this |
| `NotFound` | No such project, application, node, incident, alert, rule or trace |
| `Ambiguous` | A name matched several objects |
| `Network` | Coroot is unreachable or timed out |
| `Server` | Coroot returned an error (5xx, an error in the payload, an invalid license) |
| `Unsupported` | The server lacks the endpoint or feature (older Coroot, logs not configured) |
| `Decode` | A response this crate could not parse |
| `ResponseTooLarge` | A response body exceeded `max_response_bytes`; `response_limit()` gives the limit and `http_status()` the status |

`Ambiguous` and `NotFound` errors carry `candidates()` (for example the projects that exist). `message()` is a sentence you can show as is, and `http_status()` gives the HTTP status when there is one. The `ErrorKind` enum is `#[non_exhaustive]`, so a `match` needs a wildcard arm.

**Bounding responses.** `ClientBuilder::max_response_bytes(n)` caps every response body the client reads: data, error responses and MCP (JSON and SSE). A body whose `Content-Length` is over the limit is rejected before it is read; otherwise reading stops as soon as the limit is passed. Either way nothing is decoded and the error is `ResponseTooLarge`. The limit applies per response and counts the body bytes reqwest yields: what Coroot sends, or the decompressed bytes if you pass a decompressing client with `http_client`. There is no limit by default. `Client::execute` leaves a successful body unread, so read it with `Client::read_body` to apply the limit.

**Observing payloads.** `ClientBuilder::on_payload` is called with every JSON payload Coroot returns. Use it for debugging, recording fixtures, or showing the raw data.

## Credentials and Coroot versions

Coroot accepts a user API key (`crt_…`, created in the UI from the user menu, under API keys) or the session cookie from an email/password login.

Some data comes from Coroot's MCP endpoint, which accepts only API keys:

- traces;
- metrics;
- the richer `app_health` (`app_health_rest` works with either kind of credential).

Tested with Coroot 1.14+. Calls an older server does not support fail with `ErrorKind::Unsupported`.

## Runtime

The client uses `reqwest` with rustls and works on any Tokio runtime. It spawns no tasks, holds no background state, and opens and closes one MCP session per MCP-backed call. For many MCP calls, use `Project::mcp()` and reuse that session.

## License

MIT OR Apache-2.0.
