//! Pass case: `#[provider(BroadcastQueue(i32))]` full alias and
//! `#[provider(BQueue(i32))]` short alias should both compile.
//!
//! Guards all three Queue alias variants are accepted by the parser.

use service_daemon::provider;

#[provider(BroadcastQueue(i32))]
pub struct FullAliasQueue;

#[provider(BQueue(String))]
pub struct ShortAliasQueue;

fn main() {}
