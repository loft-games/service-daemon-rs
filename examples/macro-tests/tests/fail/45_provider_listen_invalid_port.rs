use service_daemon::provider;

#[provider(Listen(65536))]
struct TooLarge;

#[provider(Listen(-1))]
struct Negative;

#[provider(Listen(80.5))]
struct Fractional;

fn main() {}
