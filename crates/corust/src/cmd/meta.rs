//! commands, completion.

use anyhow::Result;
use clap::{Arg, ArgAction, Command, CommandFactory};
use serde_json::{Value, json};

use crate::cli::Cli;
use crate::output;

fn arg_json(a: &Arg) -> Value {
    let takes_value = matches!(a.get_action(), ArgAction::Set | ArgAction::Append);
    let mut o = json!({
        "name": a.get_id().as_str(),
        "help": a.get_help().map(|h| h.to_string()).unwrap_or_default(),
        "positional": a.is_positional(),
        "required": a.is_required_set(),
        "takes_value": takes_value,
        "repeatable": matches!(a.get_action(), ArgAction::Append),
    });
    if let Some(l) = a.get_long() {
        o["long"] = json!(format!("--{l}"));
    }
    if let Some(s) = a.get_short() {
        o["short"] = json!(format!("-{s}"));
    }
    let values: Vec<String> = a
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_string())
        .collect();
    if takes_value && !values.is_empty() {
        o["values"] = json!(values);
    }
    let defaults: Vec<String> = a
        .get_default_values()
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    if !defaults.is_empty() {
        o["default"] = json!(defaults.join(","));
    }
    if let Some(env) = a.get_env() {
        o["env"] = json!(env.to_string_lossy());
    }
    o
}

fn command_json(c: &Command, prefix: &str, global: bool) -> Value {
    let path = if prefix.is_empty() {
        c.get_name().to_string()
    } else {
        format!("{prefix} {}", c.get_name())
    };
    let args: Vec<Value> = c
        .get_arguments()
        .filter(|a| !a.is_hide_set() && a.get_id() != "help" && a.get_id() != "version")
        .filter(|a| global || !a.is_global_set())
        .map(arg_json)
        .collect();
    let subs: Vec<Value> = c
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .map(|s| command_json(s, &path, false))
        .collect();
    let mut o = json!({
        "command": path,
        "about": c.get_about().map(|h| h.to_string()).unwrap_or_default(),
    });
    let aliases: Vec<&str> = c.get_visible_aliases().collect();
    if !aliases.is_empty() {
        o["aliases"] = json!(aliases);
    }
    if !args.is_empty() {
        o[if global { "global_args" } else { "args" }] = json!(args);
    }
    if !subs.is_empty() {
        o["subcommands"] = json!(subs);
    }
    o
}

pub fn commands() -> Result<()> {
    let cmd = Cli::command();
    let mut v = command_json(&cmd, "", true);
    v["version"] = json!(env!("CARGO_PKG_VERSION"));
    v["output_formats"] = json!(["auto", "table", "wide", "json", "jsonl", "raw"]);
    v["exit_codes"] = json!({
        "0": "ok", "1": "error", "2": "usage", "3": "auth", "4": "forbidden",
        "5": "not_found", "6": "ambiguous", "7": "network", "8": "server", "9": "unsupported",
    });
    output::print_json(&v)
}

pub fn completion(shell: clap_complete::Shell) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "corust", &mut std::io::stdout());
    Ok(())
}
