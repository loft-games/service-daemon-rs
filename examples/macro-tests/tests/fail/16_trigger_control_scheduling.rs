use service_daemon::{provider, trigger};

#[provider(Notify)]
struct MySignal;

#[trigger(Event(MySignal), scheduling = Control)]
async fn control_scheduled_trigger() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
