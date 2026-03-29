use service_daemon::service;

#[service(scheduling = Unknown)] // This should fail
async fn invalid_service() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
