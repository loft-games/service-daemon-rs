//! Pass case: A sync service with #[allow_sync] compiles without warnings.

use service_daemon::{allow_sync, provider, service};

#[derive(Clone)]
#[provider(default = 42)]
pub struct MagicNumber(pub i32);

#[allow_sync]
#[service]
pub fn sync_service(num: Arc<MagicNumber>) -> anyhow::Result<()> {
    // This is intentionally sync -- fast, no I/O.
    println!("Magic: {}", num.0);
    Ok(())
}

fn main() {}
