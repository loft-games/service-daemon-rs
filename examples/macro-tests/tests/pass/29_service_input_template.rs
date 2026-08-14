use service_daemon::{service, wait_shutdown};

struct WorkerJob {
    id: u64,
}

#[service(tags = ["__macro_service_input_template__"])]
async fn on_demand_worker(#[input] job: &WorkerJob) -> anyhow::Result<()> {
    let _ = job.id;
    wait_shutdown().await;
    Ok(())
}

fn main() {}
