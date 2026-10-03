//! login, logout, whoami, config.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Context as _, Result};
use coroot_rs::util::normalize_base_url;
use coroot_rs::{Credentials, ProjectInfo};
use serde::Serialize;

use super::{Ctx, client_builder};
use crate::cli::{ConfigCmd, Global, LoginArgs};
use crate::config::{self, Config, Context};
use crate::error::{Kind, err};
use crate::output::{self, Out, Table, bold, dim, green, kv};

fn prompt(label: &str, secret: bool) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return Err(err(
            Kind::Usage,
            format!("{label} is required (no terminal to prompt on)"),
        ));
    }
    let value = if secret {
        rpassword::prompt_password(format!("{label}: "))?
    } else {
        eprint!("{label}: ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(err(Kind::Usage, format!("{label} is required")));
    }
    Ok(value)
}

pub fn redact(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 8 {
        return "****".into();
    }
    let tail: String = secret.chars().skip(n - 4).collect();
    let head: String = secret.chars().take(4).collect();
    format!("{head}…{tail}")
}

#[derive(Serialize)]
struct LoginResult {
    context: String,
    url: String,
    auth: &'static str,
    user: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
}

pub async fn login(g: &Global, args: LoginArgs, out: Out) -> Result<()> {
    let mut cfg = Config::load()?;
    let current = cfg
        .context(g.context.as_deref())
        .ok()
        .flatten()
        .map(|(n, c)| (n.to_string(), c.clone()));
    let url = match args
        .instance
        .clone()
        .or(g.url.clone())
        .or(current.as_ref().map(|(_, c)| c.url.clone()))
    {
        Some(u) => u,
        None => prompt("Coroot URL", false)?,
    };
    let base = normalize_base_url(&url).map_err(|e| err(Kind::Usage, e.to_string()))?;
    let url = base.as_str().trim_end_matches('/').to_string();

    let builder = client_builder(&url).accept_invalid_certs(g.insecure);
    let (client, kind) = if let Some(email) = &args.email {
        let password = match &args.password {
            Some(p) => p.clone(),
            None => prompt("Password", true)?,
        };
        (builder.login(email, &password).await?, "session")
    } else {
        let token = match &g.token {
            Some(t) => t.clone(),
            None => prompt("API key", true)?,
        };
        (builder.api_key(token).build()?, "token")
    };
    let user = client
        .user()
        .await
        .context("cannot verify the credentials")?;

    // Reuse an existing context for the same instance unless a name is given.
    let name = args.context_name.clone().unwrap_or_else(|| {
        cfg.contexts
            .iter()
            .find(|(_, c)| normalize_base_url(&c.url).ok().as_ref() == Some(&base))
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| config::context_name_for(&url))
    });
    let mut ctx = cfg.contexts.get(&name).cloned().unwrap_or_default();
    ctx.url = url.clone();
    ctx.insecure = g.insecure;
    ctx.token = None;
    ctx.session = None;
    match client.credentials() {
        Credentials::ApiKey(t) => ctx.token = Some(t.clone()),
        Credentials::Session(s) => ctx.session = Some(s.clone()),
        _ => {}
    }
    if let Some(p) = &g.project {
        ctx.project = Some(ProjectInfo::find(&user.projects, p)?.id.clone());
    } else if ctx
        .project
        .as_ref()
        .is_none_or(|id| !user.projects.iter().any(|p| &p.id == id))
    {
        ctx.project = (user.projects.len() == 1).then(|| user.projects[0].id.clone());
    }
    let project = ctx.project.clone();
    cfg.contexts.insert(name.clone(), ctx);
    cfg.current_context = Some(name.clone());
    cfg.save()?;

    let who = if user.name.is_empty() {
        user.email.clone()
    } else {
        format!("{} <{}>", user.name, user.email)
    };
    let res = LoginResult {
        context: name.clone(),
        url: url.clone(),
        auth: kind,
        user: who.clone(),
        project: project.clone(),
    };
    out.emit_value(&res, || {
        println!("{} logged in to {} as {}", green("✓"), bold(&url), bold(&who));
        println!("  context: {name}");
        match project.and_then(|id| user.projects.iter().find(|p| p.id == id)) {
            Some(p) => println!("  project: {} ({})", p.name, p.id),
            None if user.projects.len() > 1 => {
                println!("  {} several projects available, pick one with `corust config set-project <name>`", dim("note:"))
            }
            None => {}
        }
    })
}

pub fn logout(g: &Global) -> Result<()> {
    let mut cfg = Config::load()?;
    let name = match cfg.context(g.context.as_deref())? {
        Some((n, _)) => n.to_string(),
        None => return Err(err(Kind::Usage, "no context selected")),
    };
    if let Some(c) = cfg.contexts.get_mut(&name) {
        c.token = None;
        c.session = None;
    }
    cfg.save()?;
    eprintln!("removed credentials from context '{name}'");
    Ok(())
}

#[derive(Serialize)]
struct Whoami {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    auth: &'static str,
    id: i64,
    email: String,
    name: String,
    role: String,
    #[serde(skip_serializing_if = "is_false")]
    anonymous: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

pub async fn whoami(ctx: &Ctx) -> Result<()> {
    let user = ctx.client.user().await?;
    let cfg = Config::load()?;
    let context = cfg.current_context.clone();
    let project = ctx.project().await.ok().map(|p| p.id().to_string());
    let res = Whoami {
        url: ctx
            .client
            .base_url()
            .as_str()
            .trim_end_matches('/')
            .to_string(),
        context,
        auth: match ctx.client.credentials() {
            Credentials::ApiKey(_) => "token",
            Credentials::Session(_) => "session",
            _ => "none",
        },
        id: user.id,
        email: user.email.clone(),
        name: user.name.clone(),
        role: user.role.clone(),
        anonymous: user.anonymous,
        project: project.clone(),
    };
    ctx.out.emit_value(&res, || {
        let proj = project
            .and_then(|id| {
                user.projects
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| format!("{} ({})", p.name, p.id))
            })
            .unwrap_or_default();
        kv(&[
            ("User", format!("{} <{}>", user.name, user.email)),
            ("Role", user.role.clone()),
            ("URL", res.url.clone()),
            ("Auth", res.auth.to_string()),
            ("Context", res.context.clone().unwrap_or_default()),
            ("Project", proj),
        ]);
    })
}

#[derive(Serialize)]
struct ContextView {
    name: String,
    current: bool,
    url: String,
    auth: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    insecure: bool,
}

fn context_view(name: &str, c: &Context, current: bool) -> ContextView {
    let (auth, credential) = match (&c.token, &c.session) {
        (Some(t), _) => ("token", Some(redact(t))),
        (None, Some(s)) => ("session", Some(redact(s))),
        _ => ("none", None),
    };
    ContextView {
        name: name.to_string(),
        current,
        url: c.url.clone(),
        auth,
        credential,
        project: c.project.clone(),
        insecure: c.insecure,
    }
}

pub async fn config(g: &Global, cmd: ConfigCmd, out: Out) -> Result<()> {
    let mut cfg = Config::load()?;
    match cmd {
        ConfigCmd::Path => println!("{}", config::path()?.display()),
        ConfigCmd::View | ConfigCmd::Contexts => {
            let views: Vec<ContextView> = cfg
                .contexts
                .iter()
                .map(|(n, c)| context_view(n, c, cfg.current_context.as_deref() == Some(n)))
                .collect();
            out.emit_value(&views, || {
                let mut t = Table::new(&["", "name", "url", "auth", "project"]);
                for v in &views {
                    t.row(vec![
                        if v.current { "*".into() } else { String::new() },
                        v.name.clone(),
                        v.url.clone(),
                        format!(
                            "{} {}",
                            v.auth,
                            v.credential
                                .clone()
                                .map(|c| output::dim(&c))
                                .unwrap_or_default()
                        ),
                        v.project.clone().unwrap_or_default(),
                    ]);
                }
                if t.is_empty() {
                    println!("no contexts: run `corust login <url>`");
                } else {
                    t.print();
                }
            })?;
        }
        ConfigCmd::Use { name } => {
            if !cfg.contexts.contains_key(&name) {
                return Err(err(Kind::NotFound, format!("context '{name}' not found")));
            }
            cfg.current_context = Some(name.clone());
            cfg.save()?;
            eprintln!("switched to context '{name}'");
        }
        ConfigCmd::Delete { name } => {
            if cfg.contexts.remove(&name).is_none() {
                return Err(err(Kind::NotFound, format!("context '{name}' not found")));
            }
            if cfg.current_context.as_deref() == Some(&name) {
                cfg.current_context = cfg.contexts.keys().next().cloned();
            }
            cfg.save()?;
            eprintln!("deleted context '{name}'");
        }
        ConfigCmd::SetProject { project } => {
            let (client, _, _) = super::build_client(g, &out)?;
            let projects = client.projects().await?;
            let p = ProjectInfo::find(&projects, &project)?;
            let name = match cfg.context(g.context.as_deref())? {
                Some((n, _)) => n.to_string(),
                None => {
                    return Err(err(
                        Kind::Usage,
                        "no context selected: run `corust login` first",
                    ));
                }
            };
            if let Some(c) = cfg.contexts.get_mut(&name) {
                c.project = Some(p.id.clone());
            }
            cfg.save()?;
            eprintln!("context '{name}' now uses project {} ({})", p.name, p.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redaction() {
        assert_eq!(redact("crt_abcdefghijklmnop"), "crt_…mnop");
        assert_eq!(redact("short"), "****");
    }
}
