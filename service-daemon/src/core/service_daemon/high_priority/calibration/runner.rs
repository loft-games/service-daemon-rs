//! Opt-in experiment driver. Build and evaluate from the same recoverable snapshot.
use super::{
    artifacts::{self, Snapshot},
    evidence::{self, Expected, Verdict},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const PREFIX: &str = "core::service_daemon::high_priority::calibration::";

#[derive(Serialize, Deserialize)]
struct Manifest {
    schema: u64,
    sources: Snapshot,
    binary_sha256: String,
    rustc: String,
    cargo: String,
    profile: String,
    blocking_work_ms: u64,
    artifacts: BTreeMap<String, String>,
}

fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(fs::File::create(path)?, value)?;
    Ok(())
}

fn run_timeout(command: &mut Command, seconds: u64) -> Result<()> {
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "child failed: {status}");
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("experiment timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn driver(quick: bool) -> Result<()> {
    let output = PathBuf::from(
        std::env::var("SD_CALIBRATION_OUTPUT_DIR")
            .context("set a fresh SD_CALIBRATION_OUTPUT_DIR")?,
    );
    ensure!(output.is_absolute(), "artifact directory must be absolute");
    let test = format!(
        "{PREFIX}runner::calibration_{}",
        if quick { "quick" } else { "full" }
    );
    if std::env::var_os("SD_CALIBRATION_ARCHIVED").is_some() {
        return matrix(&output, quick);
    }
    fs::create_dir(&output).context("output must not already exist")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("workspace root")?;
    let sources = artifacts::snapshot(
        root,
        &output.join("sources"),
        &artifacts::source_paths(root)?,
    )?;
    let version = |tool: &str| -> Result<String> {
        let result = Command::new(tool).arg("--version").output()?;
        ensure!(result.status.success(), "version command failed");
        Ok(String::from_utf8(result.stdout)?.trim().into())
    };
    let mut manifest = Manifest {
        schema: 1,
        sources,
        binary_sha256: String::new(),
        rustc: version("rustc")?,
        cargo: version("cargo")?,
        profile: if quick { "quick" } else { "full" }.into(),
        blocking_work_ms: std::env::var("SD_CALIBRATION_BLOCK_MS")
            .unwrap_or_else(|_| "250".into())
            .parse()?,
        artifacts: BTreeMap::new(),
    };
    ensure!(
        (1..=1000).contains(&manifest.blocking_work_ms),
        "invalid blocking workload"
    );
    save(&output.join("manifest.json"), &manifest)?;
    let result = Command::new("cargo")
        .current_dir(output.join("sources"))
        .args([
            "test",
            "--locked",
            "--release",
            "-p",
            "service-daemon",
            "--features",
            "high-priority",
            "--lib",
            "--no-run",
            "--message-format=json",
            "-j",
            "2",
            "--target-dir",
        ])
        .arg(output.join("build"))
        .output()?;
    fs::write(output.join("build.stdout"), &result.stdout)?;
    fs::write(output.join("build.stderr"), &result.stderr)?;
    ensure!(
        result.status.success(),
        "archived build failed; inspect build.stderr"
    );
    let mut executable = None;
    for line in String::from_utf8(result.stdout)?.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] == "compiler-artifact"
            && value["target"]["name"] == "service_daemon"
            && value["profile"]["test"] == true
            && let Some(path) = value["executable"].as_str()
        {
            executable = Some(PathBuf::from(path));
        }
    }
    let binary = output.join(format!("calibration-test{}", std::env::consts::EXE_SUFFIX));
    fs::copy(executable.context("test executable not found")?, &binary)?;
    manifest.binary_sha256 = artifacts::hash(&fs::read(&binary)?);
    artifacts::verify(&output.join("sources"), &manifest.sources)?;
    save(&output.join("manifest.json"), &manifest)?;
    run_timeout(
        Command::new(binary)
            .args(["--exact", &test, "--ignored", "--nocapture"])
            .env("SD_CALIBRATION_ARCHIVED", "1")
            .env("SD_CALIBRATION_OUTPUT_DIR", &output),
        if quick { 600 } else { 4200 },
    )
}

fn matrix(output: &Path, quick: bool) -> Result<()> {
    let mut manifest: Manifest = serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
    ensure!(
        manifest.schema == 1 && manifest.profile == if quick { "quick" } else { "full" },
        "manifest profile mismatch"
    );
    let binary = std::env::current_exe()?;
    ensure!(
        artifacts::hash(&fs::read(&binary)?) == manifest.binary_sha256,
        "validator binary mismatch"
    );
    artifacts::verify(&output.join("sources"), &manifest.sources)?;
    let seconds = if quick { 10 } else { 90 };
    let mut report = String::from(
        "# Rust-native HighPriority calibration\n\nEach row is classified by the archived Rust test binary. Inconclusive is not acceptance.\n\n| Case | Verdict | Evidence |\n| --- | --- | --- |\n",
    );
    let mut accepted = true;
    for repeat in 0..if quick { 1 } else { 3 } {
        for scenario in ["healthy", "contention", "low_benefit", "external_wait"] {
            for offset in 0..3 {
                let mode = ["standard", "fixed", "adaptive"][(repeat + offset) % 3];
                let name = format!("{}-{scenario}-{mode}", repeat + 1);
                eprintln!("calibration {name}");
                let expected = Expected {
                    mode: mode.into(),
                    scenario: scenario.into(),
                    seconds,
                    blocking_work_ms: manifest.blocking_work_ms,
                };
                let raw = output.join(format!("{name}.json"));
                ensure!(
                    artifacts::hash(&fs::read(&binary)?) == manifest.binary_sha256,
                    "executable changed"
                );
                let process = run_timeout(
                    Command::new(&binary)
                        .args([
                            "--exact",
                            &format!("{PREFIX}calibration_case"),
                            "--ignored",
                            "--nocapture",
                        ])
                        .env_remove("SD_CALIBRATION_ARCHIVED")
                        .env("SD_CALIBRATION_OUTPUT", &raw)
                        .env("SD_CALIBRATION_MODE", mode)
                        .env("SD_CALIBRATION_SCENARIO", scenario)
                        .env("SD_CALIBRATION_SECONDS", seconds.to_string())
                        .env(
                            "SD_CALIBRATION_BLOCK_MS",
                            manifest.blocking_work_ms.to_string(),
                        )
                        .stdout(Stdio::from(fs::File::create(
                            output.join(format!("{name}.stdout")),
                        )?))
                        .stderr(Stdio::from(fs::File::create(
                            output.join(format!("{name}.stderr")),
                        )?)),
                    seconds + 30,
                );
                let summary = process
                    .and_then(|()| evidence::validate_json(&fs::read(&raw)?, &expected, quick));
                let summary_path = output.join(format!("{name}.summary.json"));
                match summary {
                    Ok(summary) => {
                        accepted &= summary.verdict
                            == if quick {
                                Verdict::SmokeOnly
                            } else {
                                Verdict::Pass
                            };
                        report.push_str(&format!(
                            "| {name} | {:?} | {} |\n",
                            summary.verdict, summary.reason
                        ));
                        save(&summary_path, &summary)?;
                    }
                    Err(error) => {
                        accepted = false;
                        let reason = format!("{error:#}");
                        report.push_str(&format!("| {name} | Error | {reason} |\n"));
                        save(
                            &summary_path,
                            &serde_json::json!({"verdict":"error", "reason":reason}),
                        )?;
                    }
                }
                for suffix in ["json", "summary.json", "stdout", "stderr"] {
                    let file = format!("{name}.{suffix}");
                    if output.join(&file).exists() {
                        manifest
                            .artifacts
                            .insert(file.clone(), artifacts::hash(&fs::read(output.join(file))?));
                    }
                }
                fs::write(output.join("report.md"), &report)?;
                save(&output.join("manifest.json"), &manifest)?;
            }
        }
    }
    manifest
        .artifacts
        .insert("report.md".into(), artifacts::hash(report.as_bytes()));
    save(&output.join("manifest.json"), &manifest)?;
    artifacts::verify(&output.join("sources"), &manifest.sources)?;
    for (file, hash) in &manifest.artifacts {
        ensure!(
            artifacts::hash(&fs::read(output.join(file))?) == *hash,
            "artifact changed: {file}"
        );
    }
    ensure!(
        accepted,
        "matrix contains errors or inconclusive results; preserved report is not full acceptance"
    );
    Ok(())
}

#[test]
#[ignore = "release smoke experiment; set a fresh absolute SD_CALIBRATION_OUTPUT_DIR"]
fn calibration_quick() -> Result<()> {
    driver(true)
}

#[test]
#[ignore = "54 minute release experiment; set a fresh absolute SD_CALIBRATION_OUTPUT_DIR"]
fn calibration_full() -> Result<()> {
    driver(false)
}

#[test]
#[ignore = "read-only evidence replay; supply SD_CALIBRATION_INPUT and case parameters"]
fn calibration_replay() -> Result<()> {
    let input = std::env::var("SD_CALIBRATION_INPUT")?;
    let expected = Expected {
        mode: std::env::var("SD_CALIBRATION_MODE")?,
        scenario: std::env::var("SD_CALIBRATION_SCENARIO")?,
        seconds: std::env::var("SD_CALIBRATION_SECONDS")?.parse()?,
        blocking_work_ms: std::env::var("SD_CALIBRATION_BLOCK_MS")?.parse()?,
    };
    let summary = evidence::validate_json(&fs::read(input)?, &expected, false)?;
    eprintln!("{}", serde_json::to_string_pretty(&summary)?);
    ensure!(
        summary.verdict == Verdict::Pass,
        "replayed evidence is not sufficient for acceptance"
    );
    Ok(())
}
