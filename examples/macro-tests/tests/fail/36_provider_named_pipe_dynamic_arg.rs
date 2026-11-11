use service_daemon::provider;

fn pipe_name() -> &'static str {
    r"\\.\pipe\bad"
}

#[cfg(windows)]
#[provider(NamedPipeListen(pipe_name()))]
pub struct DynamicPipeName;

fn main() {}
