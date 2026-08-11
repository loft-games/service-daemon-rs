use service_daemon::{service, wait_shutdown};

#[service(auto_start = false, tags = ["__macro_service_auto_start_false__"])]
async fn on_demand_worker() -> anyhow::Result<()> {
    wait_shutdown().await;
    Ok(())
}

#[service(auto_start = true, tags = ["__macro_service_auto_start_true__"])]
async fn explicit_auto_start_worker() -> anyhow::Result<()> {
    wait_shutdown().await;
    Ok(())
}

fn main() {}
