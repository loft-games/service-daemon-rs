use service_daemon::service;

struct Job;

#[service]
async fn worker(#[input] job: Job) -> anyhow::Result<()> {
    let _ = job;
    Ok(())
}

fn main() {}
