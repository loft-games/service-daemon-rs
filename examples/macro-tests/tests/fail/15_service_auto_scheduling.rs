use service_daemon::service;

#[service(scheduling = Auto)]
async fn auto_scheduled_service() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
