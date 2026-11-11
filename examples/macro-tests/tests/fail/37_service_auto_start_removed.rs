use service_daemon::service;

#[service(auto_start = false)]
async fn worker() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
