//! Command implementations.

mod alerts;
mod api;
mod apps;
mod auth;
mod incidents;
mod logs;
mod map;
mod meta;
mod metrics;
mod nodes;
mod overview;
mod traces;

use std::cell::OnceCell;
use std::time::Duration;

use anyhow::Result;
use coroot_rs::{Client, Credentials, Project, TimeRange};

use crate::cli::{Cli, Command, Global};
use crate::config::Config;
use crate::error::{Kind, ambiguous, err};
use crate::output::Out;

/// Where the project to operate on came from.
#[derive(Debug, Clone)]
pub enum ProjectRef {
    /// Nothing specified: use the only project, or fail if there are several.
    None,
    /// A stored project id (trusted as-is).
    Id(String),
    /// A user-supplied id or name that needs to be resolved.
    Lookup(String),
}

/// Everything a command needs to talk to Coroot and print results.
pub struct Ctx {
    pub client: Client,
    pub out: Out,
    selector: ProjectRef,
    range: TimeRange,
    project: OnceCell<Project>,
}

impl Ctx {
    /// The project commands operate on, resolved on first use.
    pub async fn project(&self) -> Result<&Project> {
        if let Some(p) = self.project.get() {
            return Ok(p);
        }
        let id = match &self.selector {
            ProjectRef::Id(id) => id.clone(),
            ProjectRef::Lookup(q) => self.client.find_project(q).await?.id().to_string(),
            ProjectRef::None => {
                let projects = self.client.projects().await?;
                match projects.as_slice() {
                    [] => {
                        return Err(err(
                            Kind::NotFound,
                            "no projects found: create one in the Coroot UI first",
                        ));
                    }
                    [p] => p.id.clone(),
                    _ => {
                        return Err(ambiguous(
                            "several projects are available, choose one with --project or `corust config set-project`",
                            projects
                                .iter()
                                .map(|p| format!("{} ({})", p.name, p.id))
                                .collect(),
                        ));
                    }
                }
            }
        };
        let p = self.client.project(id).with_range(self.range);
        Ok(self.project.get_or_init(|| p))
    }
}

pub async fn run(cli: Cli) -> Result<()> {
    let out = Out::new(cli.global.output);
    let g = cli.global;
    match cli.command {
        // Commands that work without a configured instance.
        Command::Login(args) => return auth::login(&g, args, out).await,
        Command::Logout => return auth::logout(&g),
        Command::Config(c) => return auth::config(&g, c, out).await,
        Command::Commands => return meta::commands(),
        Command::Completion { shell } => return meta::completion(shell),
        _ => {}
    }
    let (client, selector, range) = build_client(&g, &out)?;
    let ctx = Ctx {
        client,
        out,
        selector,
        range,
        project: OnceCell::new(),
    };
    match cli.command {
        Command::Whoami => auth::whoami(&ctx).await,
        Command::Projects => overview::projects(&ctx).await,
        Command::Status => overview::status(&ctx).await,
        Command::Apps(a) => apps::list(&ctx, a).await,
        Command::App(a) => apps::show(&ctx, a).await,
        Command::Nodes(a) => nodes::list(&ctx, a).await,
        Command::Node(a) => nodes::show(&ctx, a).await,
        Command::Incidents(a) => incidents::list(&ctx, a).await,
        Command::Incident(a) => incidents::show(&ctx, a).await,
        Command::Alerts(a) => alerts::alerts(&ctx, a).await,
        Command::Alert(a) => alerts::show(&ctx, a).await,
        Command::Rules(a) => alerts::rules(&ctx, a).await,
        Command::Deployments => overview::deployments(&ctx).await,
        Command::Risks => overview::risks(&ctx).await,
        Command::Map(a) => map::map(&ctx, a).await,
        Command::Logs(a) => logs::run(&ctx, a).await,
        Command::Traces(a) => traces::summary(&ctx, a).await,
        Command::Trace(a) => traces::trace(&ctx, a).await,
        Command::Query(a) => metrics::query(&ctx, a).await,
        Command::Metrics(a) => metrics::names(&ctx, a).await,
        Command::Mcp(c) => metrics::mcp(&ctx, c).await,
        Command::Api(a) => api::api(&ctx, a).await,
        Command::Open(a) => api::open(&ctx, a).await,
        Command::Login(_)
        | Command::Logout
        | Command::Config(_)
        | Command::Commands
        | Command::Completion { .. } => {
            unreachable!()
        }
    }
}

/// A client with corust's defaults: user agent, and the CORUST_TIMEOUT and
/// CORUST_MAX_RESPONSE_BYTES overrides.
pub fn client_builder(url: &str) -> coroot_rs::ClientBuilder {
    let env_u64 = |name| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let builder = Client::builder(url)
        .user_agent(concat!("corust/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(
            env_u64("CORUST_TIMEOUT").unwrap_or(120),
        ));
    match env_u64("CORUST_MAX_RESPONSE_BYTES") {
        Some(limit) => builder.max_response_bytes(limit),
        None => builder,
    }
}

/// Builds a client from flags, environment, and the selected context (in that order of precedence).
pub fn build_client(g: &Global, out: &Out) -> Result<(Client, ProjectRef, TimeRange)> {
    let cfg = Config::load()?;
    let ctx = cfg.context(g.context.as_deref())?.map(|(_, c)| c.clone());
    let url = match (&g.url, &ctx) {
        (Some(u), _) => u.clone(),
        (None, Some(c)) => c.url.clone(),
        (None, None) => {
            return Err(err(
                Kind::Usage,
                "no Coroot instance configured: run `corust login <url>` or set COROOT_URL",
            ));
        }
    };
    let normalize = |u: &str| coroot_rs::util::normalize_base_url(u).ok();
    // Credentials and defaults from the context only apply to the context's own instance.
    let ctx = ctx.filter(|c| normalize(&c.url).is_some() && normalize(&c.url) == normalize(&url));
    let credentials = if let Some(t) = &g.token {
        Credentials::ApiKey(t.clone())
    } else if let Some(t) = ctx.as_ref().and_then(|c| c.token.clone()) {
        Credentials::ApiKey(t)
    } else if let Some(s) = ctx.as_ref().and_then(|c| c.session.clone()) {
        Credentials::Session(s)
    } else {
        Credentials::None
    };
    let project = match (&g.project, ctx.as_ref().and_then(|c| c.project.clone())) {
        (Some(p), _) => ProjectRef::Lookup(p.clone()),
        (None, Some(p)) => ProjectRef::Id(p),
        (None, None) => ProjectRef::None,
    };
    let insecure = g.insecure || ctx.as_ref().is_some_and(|c| c.insecure);
    let range =
        crate::timeparse::parse_range(g.since.as_deref(), g.from.as_deref(), g.to.as_deref())
            .map_err(|e| err(Kind::Usage, e.to_string()))?;
    let mut builder = client_builder(&url)
        .credentials(credentials)
        .accept_invalid_certs(insecure);
    if let Some(slot) = out.raw_slot() {
        builder = builder.on_payload(move |p| {
            if let Ok(mut s) = slot.lock() {
                *s = Some(p.body.clone());
            }
        });
    }
    let client = builder
        .build()
        .map_err(|e| err(Kind::Usage, e.message().to_string()))?;
    Ok((client, project, range))
}
