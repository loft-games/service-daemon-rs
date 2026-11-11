use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct DownstreamFixture(PathBuf);

impl Drop for DownstreamFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn check_fixture(fixture: &Path, target: &Path, binary: &str, features: &[&str], pass: bool) {
    let output = Command::new(env!("CARGO"))
        .arg("check")
        .arg("--offline")
        .arg("--color=never")
        .arg("--manifest-path")
        .arg(fixture.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target)
        .arg("--bin")
        .arg(binary)
        .args(features)
        .output()
        .expect("run independent downstream cargo check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.success(),
        pass,
        "binary={binary}, features={features:?}\n{stderr}"
    );
    if !pass {
        assert!(
            stderr.contains("error[E0599]")
                && stderr.contains("HighPriority")
                && stderr.contains("ServiceScheduling"),
            "expected missing HighPriority variant, not an unrelated build failure:\n{stderr}"
        );
    }
}

#[test]
fn high_priority_requires_explicit_downstream_feature_opt_in() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = package.parent().expect("workspace root");
    let fixture = DownstreamFixture(
        std::env::temp_dir().join(format!("sd-high-priority-feature-{}", std::process::id())),
    );
    fs::create_dir_all(fixture.0.join("src/bin")).expect("create independent fixture");
    let dependency_path = package
        .to_str()
        .expect("UTF-8 package path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let manifest = include_str!("fixtures/high_priority_feature/Cargo.toml.template")
        .replace("__SERVICE_DAEMON_PATH__", &dependency_path);
    fs::write(fixture.0.join("Cargo.toml"), manifest).expect("write fixture manifest");
    fs::copy(workspace.join("Cargo.lock"), fixture.0.join("Cargo.lock"))
        .expect("reuse workspace dependency versions");
    let binaries = [
        "standard_isolated",
        "high_priority_direct",
        "high_priority_service",
        "high_priority_trigger",
    ];
    for binary in binaries {
        fs::copy(
            package.join(format!("tests/fixtures/high_priority_feature/{binary}.rs")),
            fixture.0.join(format!("src/bin/{binary}.rs")),
        )
        .expect("copy downstream source");
    }
    let target = workspace.join("target/high-priority-feature-contract");
    for features in [
        &[][..],
        &["--no-default-features"][..],
        &["--no-default-features", "--features", "high-priority"][..],
    ] {
        for binary in binaries {
            let pass = binary == "standard_isolated" || features.contains(&"high-priority");
            check_fixture(&fixture.0, &target, binary, features, pass);
        }
    }
}
