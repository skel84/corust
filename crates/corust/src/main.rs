mod cli;
mod cmd;
mod config;
mod error;
mod output;
mod timeparse;

use std::io::IsTerminal;

use clap::Parser;

fn main() {
    // Exit quietly when piped into `head` and the like.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if e.use_stderr() && !std::io::stdout().is_terminal() => {
            // Usage errors as JSON when a program is reading the output.
            let text = e.to_string();
            let message: Vec<&str> = text
                .lines()
                .take_while(|l| !l.trim().is_empty())
                .map(str::trim)
                .collect();
            let message = message.join(" ");
            let message = message.trim_start_matches("error: ");
            eprintln!(
                "{}",
                serde_json::json!({"error": {"kind": "usage", "message": message}})
            );
            std::process::exit(2);
        }
        Err(e) => e.exit(),
    };
    output::init_color(cli.global.no_color);
    let machine = cli.global.output.is_machine();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error::report(&e.into(), machine);
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(cmd::run(cli)) {
        error::report(&e, machine);
        std::process::exit(error::exit_code(&e));
    }
}
