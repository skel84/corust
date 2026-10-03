//! What is unhealthy in a project, and what depends on it.
//!
//! ```sh
//! COROOT_URL=https://coroot.example.com COROOT_API_KEY=crt_... \
//!     cargo run --example triage -- production
//! ```

use std::time::Duration;

use coroot_rs::{Client, Direction, IncidentQuery, LogQuery, TimeRange};

#[tokio::main(flavor = "current_thread")]
async fn main() -> coroot_rs::Result<()> {
    let url = std::env::var("COROOT_URL").expect("set COROOT_URL");
    let key = std::env::var("COROOT_API_KEY").expect("set COROOT_API_KEY");
    let project_name = std::env::args().nth(1).unwrap_or_else(|| "default".into());

    let client = Client::builder(url).api_key(key).build()?;
    let project = client
        .find_project(&project_name)
        .await?
        .with_range(TimeRange::last(Duration::from_secs(3600)));

    let problems: Vec<_> = project
        .applications()
        .await?
        .into_iter()
        .filter(|a| a.status.is_problem())
        .collect();
    let map = project.service_map().await?;

    for app in &problems {
        println!("{} {}", app.status, app.id.short());
        let health = project.app_health_rest(&app.id).await?;
        for (report, issue) in health.issues() {
            println!("  {report}: {} {}", issue.title, issue.message);
        }
        // Who is affected: everything that calls this application, two hops out.
        let mut blast = map.clone();
        blast.focus(&app.id, 2, Direction::Clients);
        let callers: Vec<String> = blast
            .nodes
            .iter()
            .filter(|n| n.distance != Some(0))
            .map(|n| n.id.short())
            .collect();
        if !callers.is_empty() {
            println!("  called by: {}", callers.join(", "));
        }
        let errors = project
            .logs(&LogQuery::new().app(&app.id).severity("error").limit(3))
            .await?;
        for e in &errors.entries {
            println!("  log: {}", e.message.trim());
        }
    }

    for i in project.incidents(&IncidentQuery::default()).await? {
        let summary = i.rca.as_ref().map(|r| r.summary.as_str()).unwrap_or("");
        println!("incident {} {} {}", i.key, i.app.short(), summary);
    }
    Ok(())
}
