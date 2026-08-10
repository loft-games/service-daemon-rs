fn main() {
    let _ = service_daemon::service_handle!(worker::<u8>);
}
