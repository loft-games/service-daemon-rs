use service_daemon::{provider, service, wait_shutdown};

#[derive(Clone, Default)]
#[provider(Dependency)]
struct Dependency;

struct Job {
    id: u64,
}

#[service(tags = ["__macro_service_input_hygiene__"])]
async fn old_context_name_collision(
    __service_invocation: Arc<Dependency>,
    #[input] job: &Job,
) -> anyhow::Result<()> {
    let _ = (&__service_invocation, job.id);
    wait_shutdown().await;
    Ok(())
}

#[service(tags = ["__macro_service_input_hygiene__"])]
async fn old_input_binding_name_collision(
    #[input] job: &Job,
    __service_input_job: Arc<Dependency>,
) -> anyhow::Result<()> {
    let _ = (job.id, &__service_input_job);
    wait_shutdown().await;
    Ok(())
}

#[service(tags = ["__macro_service_input_hygiene__"])]
async fn current_context_name_collision(
    __service_daemon_invocation: Arc<Dependency>,
    #[input] job: &Job,
) -> anyhow::Result<()> {
    let _ = (&__service_daemon_invocation, job.id);
    wait_shutdown().await;
    Ok(())
}

#[service(tags = ["__macro_service_input_hygiene__"])]
async fn current_input_binding_name_collision(
    #[input] job: &Job,
    __service_daemon_input_job: Arc<Dependency>,
) -> anyhow::Result<()> {
    let _ = (job.id, &__service_daemon_input_job);
    wait_shutdown().await;
    Ok(())
}

fn main() {}
