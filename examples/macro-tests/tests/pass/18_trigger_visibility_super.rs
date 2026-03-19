//! Pass case: Visibility is preserved through the scope module for #[trigger].

use service_daemon::TT::*;
use service_daemon::{provider, trigger};

#[provider(Notify)]
pub struct MySignal;

mod outer {
    use super::{MySignal, Event, trigger};

    mod parent {
        use super::{MySignal, Event, trigger};

        // `pub(super)` should make this reachable from `outer`.
        #[trigger(Event(MySignal))]
        pub(super) async fn on_signal() -> anyhow::Result<()> {
            Ok(())
        }

        pub fn call_from_parent() {
            let _ = on_signal;
        }
    }

    pub fn call_from_outer() {
        let _ = parent::on_signal;
    }
}

fn main() {}
