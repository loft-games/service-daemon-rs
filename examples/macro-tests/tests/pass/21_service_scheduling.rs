use service_daemon::TT::*;
use service_daemon::{provider, service, trigger};

#[service(scheduling = Isolated)]
async fn isolated_service() -> anyhow::Result<()> {
    Ok(())
}

#[service(scheduling = Standard)]
async fn standard_service() -> anyhow::Result<()> {
    Ok(())
}

#[service(scheduling = HighPriority)]
async fn high_priority_service() -> anyhow::Result<()> {
    Ok(())
}

#[provider(Notify)]
pub struct MySignal;

#[trigger(Event(MySignal))]
pub async fn on_signal_default() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Event(MySignal), scheduling = Standard)]
pub async fn on_signal_standard() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Event(MySignal), scheduling = HighPriority)]
pub async fn on_signal_high_priority() -> anyhow::Result<()> {
    Ok(())
}

#[trigger(Event(MySignal), scheduling = Isolated)]
pub async fn on_signal_isolated() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
