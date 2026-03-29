use service_daemon::service;

#[service(scheduling = Isolated)]
async fn isolated_service() -> anyhow::Result<()> {
    Ok(())
}

#[service(scheduling = Standard)]
async fn standard_service() -> anyhow::Result<()> {
    Ok(())
}

#[service(scheduling = HighPriority)]
async fn high_priority_service() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
