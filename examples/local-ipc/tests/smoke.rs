//! Compile and registry smoke test for the cross-platform local IPC example.

#[test]
fn local_ipc_example_crate_links() {
    let _name = example_local_ipc::providers::EXAMPLE_LOCAL_IPC_NAME;
}
