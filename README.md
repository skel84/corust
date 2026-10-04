# corust

A command-line client for [Coroot](https://coroot.com), built mainly for programs and AI agents and also usable by people.
It is built on [`coroot-rs`](crates/coroot-rs), an async Rust library for the same data that you can use in your own programs (see [Library](#library)).

- **Machine-first output.** When stdout is not a terminal, every command prints normalized JSON with a stable schema. On a terminal it prints tables.
- **Typed failures.** Errors are written to stderr as JSON, with a distinct exit code for each kind of failure.
- **Never blocks on input.** Nothing prompts unless stdin is a terminal.
- **Self-describing.** `corust commands` prints the whole CLI (commands, arguments, enum values, defaults, env vars) as JSON.
- **Agent-sized summaries.** Application health, PromQL, metric names and tracing use Coroot's MCP tools, which return compact, size-bounded JSON designed for LLMs.

## Install

```sh
cargo install --path crates/corust
```

## Authenticate

The recommended method is an API key, created in the Coroot UI:

```sh
corust login https://coroot.example.com --token crt_xxx
```

You can also log in with an email and password, which stores a session cookie:

```sh
COROOT_PASSWORD=... corust login https://coroot.example.com --email admin
```

Session logins cannot use the MCP-backed commands (`query`, `metrics`, `traces`, `trace`, `mcp`).

Credentials are saved as a *context* in `~/.config/corust/config.toml` (file mode 0600; `$XDG_CONFIG_HOME` is honored). You can change the location with `$CORUST_CONFIG`. A login reuses the context of the same URL; `--name` gives it a name of your choice. Manage contexts with `corust config contexts|use|set-project|delete|view|path`, check the active one with `corust whoami`, and remove its credentials with `corust logout`.

Settings are resolved in this order, highest priority first:

1. Flags: `--url`, `--token`, `-p/--project`, `--context`.
2. Environment: `COROOT_URL`, `COROOT_TOKEN`, `COROOT_PROJECT`, `CORUST_CONTEXT`, `CORUST_OUTPUT`.
3. The current context.

A context's credentials are only sent to that context's own URL. If you can only access one project, it is selected automatically; `corust projects` lists them.

Other global settings:

| Flag / variable | Effect |
|---|---|
| `--insecure` | Skip TLS certificate verification (also stored in the context at login) |
| `--no-color`, `NO_COLOR` | No ANSI colors in tables (colors are off anyway when stdout is not a terminal) |
| `CORUST_TIMEOUT` | Request timeout in seconds (default 120) |
| `CORUST_MAX_RESPONSE_BYTES` | Fail any response larger than this many bytes, before decoding it (default: no limit) |

## Commands

| Command | What it returns |
|---|---|
| `whoami`, `projects` | The current user, URL, auth method and project; the projects you can access, with the current one marked (`current` in JSON) |
| `status` | Data-source health (Prometheus, node agent, kube-state-metrics), app and node counts, firing alerts and open incidents |
| `apps [filter] [--problems] [--status S] [-c category] [-n ns]` | Applications with their health signals |
| `app <name> [--rest]` | Vitals (rps, latency percentiles, cpu, memory), failed inspections, log patterns, dependencies and clients |
| `nodes [filter]`, `node <name>` | Nodes, and a node's audit checks with the latest metric values |
| `incidents [--state open\|resolved\|any] [--app A] [-l N]`, `incident <key>` | SLO incidents, open ones first, then newest first (default: any state, 50), with root-cause analysis, propagation map and burn rates |
| `alerts [--state firing\|resolved\|any] [--app A] [-s text] [-l N]` | Alerts (default: firing, including suppressed, 100) |
| `alerts resolve\|suppress\|reopen <ids…\|->` | Alert actions; `-` reads ids from stdin |
| `alert <id>` | One alert with its details |
| `rules [list\|get\|export\|create -f\|update -f\|enable\|disable\|delete]` | Alerting rules. `-f` takes a JSON file or `-` for stdin; `export` prints YAML (`{"yaml": …}` in JSON mode) |
| `map [app] [-d N] [--direction dependencies\|clients\|both] [--problems]` | The service map as a graph, or one application's neighborhood up to N hops (default 1, both directions) |
| `deployments`, `risks` | Deployments with their impact, and availability and security risks |
| `logs [app] [-l sev] [-s text] [-x text] [-F k=v] [--trace-id] [--source agent\|otel] [-n N] [-f] [--interval S] [--patterns]` | The newest N log entries (default 100), printed oldest first. Without an app, searches the whole project. `-F` takes `k=v`, `k!=v`, `k~regex` or `k!~regex` |
| `traces [--service] [--span] [--errors \| --slow 500ms]`, `trace <id>` | Per-endpoint summary, top errors, slow-tail analysis, and one trace as a span tree |
| `query '<promql>' [--step] [-l N]` | A PromQL range query |
| `metrics [pattern] [-l N]` | Metric names (default 500). A plain word is a substring match; anything else is an RE2 regex |
| `mcp tools`, `mcp call <tool> [k=v… \| '{json}'] [--no-project]` | Any Coroot MCP tool, called directly. Values are parsed as JSON when possible; the current project is selected first unless `--no-project` |
| `api <path> [-X M] [-q k=v] [-d json\|@file\|@-]` | An authenticated request to the raw API. `{project}` is replaced with the current project id |
| `open [app\|incident/KEY\|alert/ID\|node/N\|view] [-b]` | The UI link (default: the applications view), or opens it in a browser with `-b`. Views: `applications`, `incidents`, `alerts`, `map`, `nodes`, `kubernetes`, `traces`, `logs`, `costs`, `anomalies`, `risks`, `dashboards`, `deployments` |
| `commands`, `completion <shell>` | The CLI description as JSON, and shell completions |

### Names and time ranges

Applications can be named by full id (`cluster:ns:Kind:name`), `ns:Kind:name`, `ns/name`, `Kind/name`, `name`, or a unique substring. If a name matches more than one application, the command exits with code 6 and lists the candidates.

The time window applies to every command. Use either `--since 6h`, or `--from` and `--to`. Accepted time formats are:

- `now-1h`
- RFC 3339
- `"2026-01-02 15:04"`
- `2026-01-02`
- epoch seconds or milliseconds

If you don't set a window, the server's default applies (usually 1h).

## Output contract

`-o` / `CORUST_OUTPUT` takes one of these values:

| Format | Meaning |
|---|---|
| `auto` (default) | `table` on a terminal, `json` otherwise |
| `json` | Normalized JSON: pretty on a terminal, compact one-line JSON when piped |
| `jsonl` | One JSON object per line; lists are split into their elements |
| `table` / `wide` | Human-readable; `wide` adds columns |
| `raw` | The unmodified payload of the last Coroot request (the full `{context, data}` envelope for REST views), for when you need a field the normalized form drops |

The normalized JSON follows these rules:

- Keys are snake_case.
- Timestamps are RFC 3339 UTC strings.
- Durations are whole seconds, in fields named `*_seconds`.
- Statuses are `ok|info|warning|critical|unknown`.
- Optional values are omitted rather than null. The exceptions are fields that are always present and are `null` when unknown: `opened_at`, `status.prometheus` and `status.node_agent` (null when not configured), the impact percentages of an incident's `slo`, and gaps in `query` series values.
- Missing lists are `[]` and missing maps are `{}`.
- UI widget trees and raw chart data are dropped; `app` keeps compact chart summaries.

`apps` keeps every known signal present in Coroot's response, including `ok` and
`unknown` signals with no value. For example, `"signals":{"cpu":{"status":"ok"},
"memory":{"status":"unknown"}}` differs from an absent signal. Empty values are
omitted from JSON; absent signals stay absent. A present null/malformed signal or
missing/unrecognized status becomes `unknown`, never healthy by default.
This adds entries that earlier JSON/JSONL output omitted; filter by the signal's
status instead of treating its presence as a problem. Tables still leave empty
values blank, and raw output remains unchanged.

Applications are always described the same way:

```json
{"id":"c1:prod:Deployment:api","name":"api","namespace":"prod","kind":"Deployment","cluster_id":"c1"}
```

Some sample records:

```jsonc
// corust alerts
{"id":"a5upegkn1a5x","rule_id":"new-log-patterns","rule_name":"Log errors",
 "app":{"id":"…","name":"web","kind":"Unknown","cluster_id":"mpacxnut"},
 "severity":"warning","state":"firing","suppressed":false,"summary":"new error in the logs (41 messages)",
 "opened_at":"2026-10-03T09:09:25Z","duration_seconds":786,"report":"Logs"}

// corust logs web -o jsonl
{"timestamp":"2026-10-03T09:19:13.260Z","app_id":"…","severity":"error","message":"…",
 "attributes":{"host.name":"node-1","pattern.hash":"137c4a…","service.name":"…"}}
```

`map` returns `{nodes, edges}`. An edge goes from a client to the service it calls:

```jsonc
{"nodes":[{"id":"…:web","name":"web","kind":"Unknown","cluster_id":"c1","cluster":"default","category":"application",
           "status":"ok","labels":{"instances":1},"indicators":{"cpu":"ok","logs":"warning","…":"…"},"distance":1}],
 "edges":[{"from":"…:loadgen","to":"…:web","status":"ok","rps":0.333,"latency_seconds":0.0006,
           "sent_bytes_per_second":35.0,"received_bytes_per_second":347.0}]}
```

Coroot renders link stats as display text. `rps` is exact, but latency and traffic are recovered from that text, so they carry its rounding. A broken connection carries an `issue` string. `distance` is the number of hops from the focused application.

Traces keep Coroot's units: a span's `timestamp` is epoch milliseconds and its `duration` is in milliseconds, and the `duration_quantiles` of `traces` (p50, p95, p99) are in seconds.

`app`, `query`, `metrics`, `traces` and `trace` follow the schema of Coroot's MCP tools, with the same normalization as everything else (missing lists are `[]`, missing statuses are `unknown`). List results there take the form `{total, returned, truncated, hint, items}`. `mcp call` prints the tool's answer unchanged.

With session auth (MCP needs an API key), or with `--rest`, `app` reads the REST API instead. The result has the same shape without `vitals`, `charts` and `log_patterns`. Connection `rps` and latency there are recovered from Coroot's rounded display text, and `clients` also carry them.

`rules get` returns the rule in Coroot's own schema, so you can edit it and pass it back to `rules update <id> -f -`.

### Errors and exit codes

Errors are written to stderr. When output is machine-readable, the error is a single JSON line:

```json
{"error":{"kind":"ambiguous","message":"'corust' matches several applications, be more specific:","candidates":["…","…"]}}
```

| Code | Kind | Meaning |
|---|---|---|
| 0 | | Success |
| 1 | `error` | Unclassified error |
| 2 | `usage` | Invalid arguments or configuration, including clap parse errors |
| 3 | `auth` | Missing or rejected credentials, or an API key is required |
| 4 | `forbidden` | The user's role does not allow this |
| 5 | `not_found` | No such project, application, node, alert, incident, trace or MCP tool |
| 6 | `ambiguous` | The name matched several objects; see `candidates` |
| 7 | `network` | Coroot is unreachable |
| 8 | `server` | Coroot returned an error |
| 9 | `unsupported` | This Coroot version lacks the endpoint |
| 10 | `response_too_large` | A response exceeded `CORUST_MAX_RESPONSE_BYTES` |

## Examples for agents and scripts

```sh
# Triage: what is unhealthy right now?
corust status
corust apps --problems
corust incidents --state open

# Drill into one application
corust app checkout
corust logs checkout -l error -n 50 --since 30m
corust logs checkout --patterns
corust traces --service checkout --errors

# Blast radius: everything that (transitively) calls the database
corust map postgres --direction clients --depth 3

# Correlate by trace id
corust trace 4bf92f3577b34da6a3ce929d0e0e4736 --since 24h

# Metrics
corust metrics tcp
corust query 'sum by (container_id) (rate(container_resources_cpu_usage_seconds_total[5m]))' --since 1h -l 10

# Stream logs as JSON lines (follows Coroot's max_ts cursor, so polls neither overlap nor
# leave gaps; a poll is capped at 10,000 entries and warns on stderr if it hits the cap)
corust logs -l error -f -o jsonl | your-consumer

# Resolve every firing alert of one app
corust alerts --app web | jq -r '.[].id' | corust alerts resolve -
```

## Library

[`coroot-rs`](crates/coroot-rs) is the async client the CLI is built on: typed models for everything above, the same normalized JSON when serialized, and errors with a kind to match on. The CLI's exit codes are those kinds (`InvalidInput` is `usage`; `Decode`, a response the library could not parse, is the unclassified code 1).

```rust
use std::time::Duration;
use coroot_rs::{Client, Direction, IncidentQuery, LogQuery, TimeRange};

let client = Client::builder("https://coroot.example.com").api_key(key).build()?;
let project = client
    .find_project("production")
    .await?
    .with_range(TimeRange::last(Duration::from_secs(3600)));

let unhealthy: Vec<_> = project
    .applications()
    .await?
    .into_iter()
    .filter(|a| a.status.is_problem())
    .collect();

let db = project.resolve_app("postgres").await?;
let mut map = project.service_map().await?;
map.focus(&db, 2, Direction::Clients); // the blast radius

let errors = project.logs(&LogQuery::new().app(&db).severity("error").limit(50)).await?;
let incidents = project.incidents(&IncidentQuery::default()).await?;
```

See [its README](crates/coroot-rs/README.md) for the full API.

## Development

The repository is a Cargo workspace: `crates/coroot-rs` (library, MIT OR Apache-2.0) and `crates/corust` (CLI, Apache-2.0).

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
