use service_daemon::{TT, trigger};

struct Event;

#[trigger(TT::Queue(Event))]
async fn worker(#[input] event: Event) -> anyhow::Result<()> {
    let _ = event;
    Ok(())
}

fn main() {}
