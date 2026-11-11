use service_daemon::service;

struct Job;
struct Connection;

#[service]
async fn worker(#[input] job: &Job, #[input] connection: &Connection) -> anyhow::Result<()> {
    let _ = (job, connection);
    Ok(())
}

fn main() {}
