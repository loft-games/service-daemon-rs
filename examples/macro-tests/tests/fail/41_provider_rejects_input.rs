use service_daemon::provider;

struct Dependency;

#[provider]
fn value(#[input] dependency: Dependency) -> usize {
    let _ = dependency;
    1
}

fn main() {}
