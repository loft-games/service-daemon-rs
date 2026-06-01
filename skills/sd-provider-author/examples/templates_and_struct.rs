// Template providers (signal / queue / socket sources) and a composed struct
// provider. Template names: Notify, Event, Queue, BQueue, BroadcastQueue,
// Listen, UnixListen, UnixConnect.
use service_daemon::provider;
use std::sync::Arc;

// Signal source (no payload) — pair with #[trigger(Event(MySignal))].
#[provider(Notify)]
pub struct MySignal;

// Broadcast queue carrying String payloads — pair with #[trigger(Queue(TaskQueue))].
#[provider(Queue(String))]
pub struct TaskQueue;

// Bounded queue (capacity must be > 0).
#[provider(Queue(ComplexJob), capacity = 500)]
pub struct JobQueue;

// TCP listener; env overrides the bind address. Named attrs go OUTSIDE the parens.
#[provider(Listen("0.0.0.0:8080"), env = "LISTEN_ADDR")]
pub struct ApiListener;

// Unix socket peer connection, initialized eagerly.
#[provider(UnixConnect("/run/peer/sock"), env = "PEER_SOCK", eager = true)]
pub struct PeerSocket;

// Struct provider composed from other providers. Arc<_> fields are resolved as
// dependencies; any non-Arc field must implement Default.
#[provider]
pub struct AppConfig {
    pub port: Arc<Port>,
    pub db_url: Arc<DbUrl>,
}
