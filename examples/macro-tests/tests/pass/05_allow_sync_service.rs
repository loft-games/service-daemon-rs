//! Pass case: A sync service with #[allow(sync_handler)] compiles without warnings.

use service_daemon::{provider, service};

#[derive(Clone)]
#[provider(42)]
pub struct MagicNumber(pub i32);

#[service]
#[allow(sync_handler)]
pub fn sync_service(num: Arc<MagicNumber>) -> anyhow::Result<()> {
    // This is intentionally sync -- fast, no I/O.
    println!("Magic: {}", num);
    Ok(())
}

fn main() {}
