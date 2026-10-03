//! Command-line interface definition.

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::output::Format;

const AFTER_HELP: &str = "\
Output: tables on a terminal, normalized JSON when piped (override with -o).
Exit codes: 0 ok, 1 error, 2 usage, 3 auth, 4 forbidden, 5 not found, 6 ambiguous name,
            7 network, 8 server error, 9 unsupported by this Coroot version.
Run `corust commands` for a machine-readable description of all commands.";

#[derive(Parser, Debug)]
#[command(name = "corust", version, about = "Command-line client for Coroot", after_help = AFTER_HELP)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Clone)]
pub struct Global {
    /// Configuration context to use
    #[arg(long, global = true, env = "CORUST_CONTEXT", value_name = "NAME")]
    pub context: Option<String>,
    /// Coroot URL (overrides the context)
    #[arg(long, global = true, env = "COROOT_URL", value_name = "URL")]
    pub url: Option<String>,
    /// API key (overrides the context)
    #[arg(
        long,
        global = true,
        env = "COROOT_TOKEN",
        hide_env_values = true,
        value_name = "KEY"
    )]
    pub token: Option<String>,
    /// Project name or id
    #[arg(
        short,
        long,
        global = true,
        env = "COROOT_PROJECT",
        value_name = "PROJECT"
    )]
    pub project: Option<String>,
    /// Output format
    #[arg(short, long, global = true, value_enum, default_value_t = Format::Auto, env = "CORUST_OUTPUT")]
    pub output: Format,
    /// Time window ending now (or at --to), e.g. 15m, 6h, 2d
    #[arg(long, global = true, value_name = "DURATION")]
    pub since: Option<String>,
    /// Start of the time window: now-1h, RFC 3339, "2026-01-02 15:04", epoch
    #[arg(long, global = true, value_name = "TIME")]
    pub from: Option<String>,
    /// End of the time window (default: now)
    #[arg(long, global = true, value_name = "TIME")]
    pub to: Option<String>,
    /// Disable colors
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Skip TLS certificate verification
    #[arg(long, global = true)]
    pub insecure: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Save credentials for a Coroot instance as a context
    Login(LoginArgs),
    /// Remove credentials from a context
    Logout,
    /// Show the authenticated user
    Whoami,
    /// Manage contexts and settings
    #[command(subcommand)]
    Config(ConfigCmd),
    /// List projects
    Projects,
    /// Project health: data sources, agents, firing alerts, open incidents
    Status,
    /// List applications with their health
    #[command(visible_alias = "applications")]
    Apps(AppsArgs),
    /// Application health: inspections, issues, dependencies and clients
    #[command(visible_alias = "application")]
    App(AppArgs),
    /// List nodes
    Nodes(NodesArgs),
    /// Node audit report
    Node(NodeArgs),
    /// List SLO incidents
    Incidents(IncidentsArgs),
    /// Incident details with root cause analysis
    Incident(IncidentArgs),
    /// List alerts, or act on them
    Alerts(AlertsArgs),
    /// Alert details
    Alert(AlertArgs),
    /// Manage alerting rules
    Rules(RulesArgs),
    /// Recent deployments and their impact
    Deployments,
    /// Availability and security risks
    Risks,
    /// Service map: applications and the connections between them, as a graph
    Map(MapArgs),
    /// Query logs of an application or the whole project
    Logs(LogsArgs),
    /// Distributed tracing: per-endpoint summary, errors, or slow-tail analysis
    Traces(TracesArgs),
    /// Show a single trace as a span tree
    Trace(TraceArgs),
    /// Run a PromQL range query
    Query(QueryArgs),
    /// List metric names
    Metrics(MetricsArgs),
    /// Call Coroot's MCP tools directly
    #[command(subcommand)]
    Mcp(McpCmd),
    /// Make an authenticated request to the Coroot API
    Api(ApiArgs),
    /// Print (or open) the Coroot UI link for an object
    Open(OpenArgs),
    /// Describe all commands and arguments as JSON
    Commands,
    /// Generate shell completions
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Coroot URL, e.g. https://coroot.example.com (or use --url); authenticate with
    /// --token (an API key from the Coroot UI, recommended) or --email/--password
    #[arg(id = "instance", value_name = "URL")]
    pub instance: Option<String>,
    /// Log in with email and password instead of an API key
    #[arg(long)]
    pub email: Option<String>,
    /// Password (prompted when omitted; also read from COROOT_PASSWORD)
    #[arg(long, env = "COROOT_PASSWORD", hide_env_values = true)]
    pub password: Option<String>,
    /// Name of the context to create or update
    #[arg(long = "name", value_name = "NAME")]
    pub context_name: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Show the effective configuration (secrets redacted)
    View,
    /// List contexts
    Contexts,
    /// Switch the current context
    Use { name: String },
    /// Set the default project of the current context
    SetProject { project: String },
    /// Delete a context
    Delete { name: String },
    /// Print the config file path
    Path,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum MinStatus {
    Ok,
    Info,
    Warning,
    Critical,
}

impl MinStatus {
    pub fn status(self) -> coroot_rs::Status {
        match self {
            MinStatus::Ok => coroot_rs::Status::Ok,
            MinStatus::Info => coroot_rs::Status::Info,
            MinStatus::Warning => coroot_rs::Status::Warning,
            MinStatus::Critical => coroot_rs::Status::Critical,
        }
    }
}

#[derive(Args, Debug)]
pub struct AppsArgs {
    /// Only applications whose id contains this text
    pub filter: Option<String>,
    /// Only applications in these categories (repeatable)
    #[arg(short, long)]
    pub category: Vec<String>,
    /// Only applications in this namespace
    #[arg(short, long)]
    pub namespace: Option<String>,
    /// Only applications with at least this status
    #[arg(long, value_enum)]
    pub status: Option<MinStatus>,
    /// Shortcut for --status warning
    #[arg(long, conflicts_with = "status")]
    pub problems: bool,
}

#[derive(Args, Debug)]
pub struct MapArgs {
    /// Only the neighborhood of this application
    pub app: Option<String>,
    /// With an application: how many hops to follow
    #[arg(short, long, default_value_t = 1, requires = "app")]
    pub depth: usize,
    /// With an application: follow its dependencies, its clients, or both
    #[arg(long, value_enum, default_value_t = MapDirection::Both, requires = "app")]
    pub direction: MapDirection,
    /// Only connections and applications with warning or critical status
    #[arg(long)]
    pub problems: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapDirection {
    /// Services this application calls (and theirs, with --depth)
    Dependencies,
    /// Services calling this application (its blast radius)
    Clients,
    Both,
}

impl From<MapDirection> for coroot_rs::Direction {
    fn from(d: MapDirection) -> Self {
        match d {
            MapDirection::Dependencies => coroot_rs::Direction::Dependencies,
            MapDirection::Clients => coroot_rs::Direction::Clients,
            MapDirection::Both => coroot_rs::Direction::Both,
        }
    }
}

#[derive(Args, Debug)]
pub struct AppArgs {
    /// Application: name, namespace/name, Kind/name, or full id
    pub app: String,
    /// Use the REST API instead of MCP (more detail, larger output)
    #[arg(long)]
    pub rest: bool,
}

#[derive(Args, Debug)]
pub struct NodesArgs {
    /// Only nodes whose name contains this text
    pub filter: Option<String>,
}

#[derive(Args, Debug)]
pub struct NodeArgs {
    /// Node name or cluster_id:name
    pub node: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum IncidentState {
    Open,
    Resolved,
    Any,
}

impl From<IncidentState> for coroot_rs::StateFilter {
    fn from(s: IncidentState) -> Self {
        match s {
            IncidentState::Open => coroot_rs::StateFilter::Open,
            IncidentState::Resolved => coroot_rs::StateFilter::Resolved,
            IncidentState::Any => coroot_rs::StateFilter::Any,
        }
    }
}

#[derive(Args, Debug)]
pub struct IncidentsArgs {
    /// Filter by state
    #[arg(long, value_enum, default_value_t = IncidentState::Any)]
    pub state: IncidentState,
    /// Only incidents of this application
    #[arg(long)]
    pub app: Option<String>,
    /// Maximum number of incidents
    #[arg(short, long, default_value_t = 50)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct IncidentArgs {
    /// Incident key
    pub key: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AlertState {
    Firing,
    Resolved,
    Any,
}

impl From<AlertState> for coroot_rs::StateFilter {
    fn from(s: AlertState) -> Self {
        match s {
            AlertState::Firing => coroot_rs::StateFilter::Open,
            AlertState::Resolved => coroot_rs::StateFilter::Resolved,
            AlertState::Any => coroot_rs::StateFilter::Any,
        }
    }
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AlertsArgs {
    #[command(subcommand)]
    pub action: Option<AlertsCmd>,
    /// Filter by state
    #[arg(long, value_enum, default_value_t = AlertState::Firing)]
    pub state: AlertState,
    /// Only alerts of this application
    #[arg(long)]
    pub app: Option<String>,
    /// Full-text search
    #[arg(short, long)]
    pub search: Option<String>,
    /// Maximum number of alerts
    #[arg(short, long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Subcommand, Debug)]
pub enum AlertsCmd {
    /// Manually resolve alerts (triggers resolve notifications)
    Resolve { ids: Vec<String> },
    /// Suppress alerts
    Suppress { ids: Vec<String> },
    /// Reopen resolved or suppressed alerts
    Reopen { ids: Vec<String> },
}

#[derive(Args, Debug)]
pub struct AlertArgs {
    /// Alert id
    pub id: String,
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RulesArgs {
    #[command(subcommand)]
    pub action: Option<RulesCmd>,
}

#[derive(Subcommand, Debug)]
pub enum RulesCmd {
    /// List rules (default)
    List,
    /// Show a rule as JSON
    Get { id: String },
    /// Export all rules as YAML for the config file
    Export,
    /// Create a rule from a JSON file ("-" for stdin)
    Create {
        #[arg(short, long)]
        file: String,
    },
    /// Replace a rule with the contents of a JSON file ("-" for stdin)
    Update {
        id: String,
        #[arg(short, long)]
        file: String,
    },
    /// Enable a rule
    Enable { id: String },
    /// Disable a rule
    Disable { id: String },
    /// Delete a rule (resolves its alerts)
    Delete { id: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum LogSource {
    Agent,
    Otel,
}

impl From<LogSource> for coroot_rs::LogSource {
    fn from(s: LogSource) -> Self {
        match s {
            LogSource::Agent => coroot_rs::LogSource::Agent,
            LogSource::Otel => coroot_rs::LogSource::Otel,
        }
    }
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Application (omit to search the whole project)
    pub app: Option<String>,
    /// Severities to include (repeatable): debug, info, warning, error, fatal, ...
    #[arg(short = 'l', long = "severity")]
    pub severity: Vec<String>,
    /// Only messages containing all words of TEXT (repeatable)
    #[arg(short, long, value_name = "TEXT")]
    pub search: Vec<String>,
    /// Drop messages containing all words of TEXT (repeatable)
    #[arg(short = 'x', long, value_name = "TEXT")]
    pub exclude: Vec<String>,
    /// Only entries of this trace
    #[arg(long)]
    pub trace_id: Option<String>,
    /// Filter on an attribute or field: NAME=VALUE, NAME!=VALUE, NAME~REGEX, NAME!~REGEX (repeatable)
    #[arg(short = 'F', long = "filter", value_name = "EXPR")]
    pub filters: Vec<String>,
    /// Maximum number of entries
    #[arg(short = 'n', long, default_value_t = 100)]
    pub limit: usize,
    /// Log source
    #[arg(long, value_enum)]
    pub source: Option<LogSource>,
    /// Keep polling for new entries
    #[arg(short, long)]
    pub follow: bool,
    /// Poll interval for --follow, in seconds
    #[arg(long, default_value_t = 5, value_name = "SECONDS")]
    pub interval: u64,
    /// Show log patterns (grouped messages) instead of entries
    #[arg(long, conflicts_with = "follow")]
    pub patterns: bool,
}

#[derive(Args, Debug)]
pub struct TracesArgs {
    /// Only spans of this service (service.name)
    #[arg(long)]
    pub service: Option<String>,
    /// Only spans with this name (e.g. "GET /cart")
    #[arg(long)]
    pub span: Option<String>,
    /// Show the most frequent errors instead of the summary
    #[arg(long, conflicts_with = "slow")]
    pub errors: bool,
    /// Explain the slow tail: where time goes in traces slower than DURATION (e.g. 500ms)
    #[arg(long, value_name = "DURATION")]
    pub slow: Option<String>,
}

#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Trace id
    pub trace_id: String,
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// PromQL expression
    pub query: String,
    /// Step in seconds (widened automatically to at most ~120 points)
    #[arg(long)]
    pub step: Option<u64>,
    /// Maximum number of series
    #[arg(short, long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct MetricsArgs {
    /// RE2 regex on metric names
    pub pattern: Option<String>,
    /// Maximum number of names
    #[arg(short, long, default_value_t = 500)]
    pub limit: usize,
}

#[derive(Subcommand, Debug)]
pub enum McpCmd {
    /// List MCP tools with their input schemas
    Tools,
    /// Call a tool: corust mcp call list_alerts state=firing limit=5
    Call {
        tool: String,
        /// Arguments as KEY=VALUE (values parsed as JSON when possible) or a single JSON object
        args: Vec<String>,
        /// Do not select the current project first
        #[arg(long)]
        no_project: bool,
    },
}

#[derive(Args, Debug)]
pub struct ApiArgs {
    /// Path relative to the Coroot URL, e.g. /api/user or api/project/{project}/incidents
    /// ({project} is replaced with the current project id)
    pub path: String,
    /// HTTP method
    #[arg(short = 'X', long, default_value = "GET")]
    pub method: String,
    /// Query parameters KEY=VALUE (repeatable)
    #[arg(short, long = "query", value_name = "KEY=VALUE")]
    pub query: Vec<String>,
    /// Request body: JSON string, @file, or @- for stdin
    #[arg(short, long)]
    pub data: Option<String>,
}

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// What to open: an application name, `incident/<key>`, `alert/<id>`, `node/<name>`,
    /// or a view: applications, incidents, alerts, nodes, logs, traces, deployments, risks, costs
    pub target: Option<String>,
    /// Open the link in a browser instead of printing it
    #[arg(short, long)]
    pub browser: bool,
}
