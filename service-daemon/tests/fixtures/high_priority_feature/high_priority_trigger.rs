use service_daemon::TT::*;
use service_daemon::{provider, trigger};

#[provider(Notify)]
pub struct Signal;

#[trigger(Event(Signal), scheduling = HighPriority)]
async fn high_priority_trigger() -> anyhow::Result<()> {
    Ok(())
}

fn main() {}
