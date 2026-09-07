use service_daemon::service;

#[service(scheduling = HighPriority)]
async fn high_priority_service() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
