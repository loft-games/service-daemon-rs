//! Pass case: shared provider env arguments accept const and static string paths.

use service_daemon::provider;

const DATABASE_ENV: &str = "SERVICE_DAEMON_RS_MACRO_DATABASE_URL";
const API_LISTEN_ENV: &str = "SERVICE_DAEMON_RS_MACRO_API_LISTEN_ADDR";
static API_KEY_ENV: &str = "SERVICE_DAEMON_RS_MACRO_API_KEY";
static STATIC_LOCAL_IPC_NAME: &str = "service-daemon-rs-macro-static-local-ipc";
static STATIC_LOCAL_IPC_ENV: &str = "SERVICE_DAEMON_RS_MACRO_STATIC_LOCAL_IPC";

#[derive(Clone)]
#[provider("postgres://localhost/app", env = DATABASE_ENV)]
pub struct DatabaseUrl(pub String);

#[derive(Clone)]
#[provider(env = API_KEY_ENV)]
pub struct ApiKey(pub String);

#[provider(Listen("127.0.0.1:0"), env = API_LISTEN_ENV)]
pub struct ApiListener;

#[provider(LocalIpcListen(STATIC_LOCAL_IPC_NAME), env = STATIC_LOCAL_IPC_ENV)]
pub struct StaticLocalIpcServer;

fn main() {}
