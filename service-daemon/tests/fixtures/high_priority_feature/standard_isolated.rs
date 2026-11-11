use service_daemon::TT::*;
use service_daemon::{ServiceScheduling, provider, service, trigger};

#[service(scheduling = Standard)]
async fn standard_service() -> anyhow::Result<()> {
    Ok(())
}

#[service(scheduling = Isolated)]
async fn isolated_service() -> anyhow::Result<()> {
    Ok(())
}

#[provider(Notify)]
pub struct Signal;

#[trigger(Event(Signal), scheduling = Standard)]
async fn standard_trigger() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Event(Signal), scheduling = Isolated)]
async fn isolated_trigger() -> anyhow::Result<()> {
    Ok(())
}

fn main() {
    let _ = (ServiceScheduling::Standard, ServiceScheduling::Isolated);
}
