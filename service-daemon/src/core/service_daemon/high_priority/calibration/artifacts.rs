//! Recoverable, allowlisted build inputs and content-addressed artifact integrity.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub files: BTreeMap<String, String>,
    pub id: String,
}

pub(super) fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn checked(root: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "invalid archive path"
    );
    let path = root.join(relative);
    ensure!(
        path.canonicalize()? == root.canonicalize()?.join(relative),
        "symlink or external archive input"
    );
    Ok(path)
}

pub(super) fn snapshot(root: &Path, output: &Path, paths: &[PathBuf]) -> Result<Snapshot> {
    fs::create_dir(output)?;
    let mut files = BTreeMap::new();
    for relative in paths {
        let bytes = fs::read(checked(root, relative)?)?;
        let destination = output.join(relative);
        fs::create_dir_all(destination.parent().context("missing parent")?)?;
        fs::write(destination, &bytes)?;
        ensure!(
            files
                .insert(
                    relative
                        .to_str()
                        .context("non UTF-8 source path")?
                        .replace('\\', "/"),
                    hash(&bytes)
                )
                .is_none(),
            "duplicate input"
        );
    }
    let id = hash(&serde_json::to_vec(&files)?);
    Ok(Snapshot { files, id })
}

pub(super) fn verify(root: &Path, snapshot: &Snapshot) -> Result<()> {
    ensure!(
        hash(&serde_json::to_vec(&snapshot.files)?) == snapshot.id,
        "source manifest identity mismatch"
    );
    for (path, expected) in &snapshot.files {
        ensure!(
            hash(&fs::read(checked(root, Path::new(path))?)?) == *expected,
            "artifact changed: {path}"
        );
    }
    Ok(())
}

pub(super) fn source_paths(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, relative: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
        let directory = root.join(relative);
        if !directory.exists() {
            return Ok(());
        }
        checked(root, relative)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            ensure!(!entry.file_type()?.is_symlink(), "symlink in source tree");
            if entry.file_type()?.is_dir() {
                visit(root, &path, paths)?;
            } else if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("rs" | "toml" | "template" | "stderr")
            ) {
                paths.push(path);
            }
        }
        Ok(())
    }
    let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let mut paths = Vec::new();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "README.md",
        "rust-toolchain.toml",
        "rust-toolchain",
        ".cargo/config.toml",
        ".cargo/config",
    ] {
        if root.join(name).is_file() {
            paths.push(PathBuf::from(name));
        }
    }
    for member in workspace["workspace"]["members"]
        .as_array()
        .context("missing workspace members")?
    {
        let member = Path::new(member.as_str().context("invalid workspace member")?);
        checked(root, member)?;
        for name in ["Cargo.toml", "build.rs"] {
            if root.join(member).join(name).is_file() {
                paths.push(member.join(name));
            }
        }
        for name in ["src", "tests"] {
            visit(root, &member.join(name), &mut paths)?;
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("sd-archive-{}", uuid::Uuid::now_v7()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn untracked_source_is_recoverable_after_original_is_deleted() {
        let temp = Temp::new();
        let root = temp.0.join("repo");
        fs::create_dir(&root).unwrap();
        assert!(
            std::process::Command::new("git")
                .arg("init")
                .arg(&root)
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::write(root.join("harness.rs"), "original untracked experiment").unwrap();
        let output = temp.0.join("archive");
        let saved = snapshot(&root, &output, &[PathBuf::from("harness.rs")]).unwrap();
        fs::write(root.join("harness.rs"), "changed").unwrap();
        fs::remove_file(root.join("harness.rs")).unwrap();
        verify(&output, &saved).unwrap();
        assert_eq!(
            fs::read_to_string(output.join("harness.rs")).unwrap(),
            "original untracked experiment"
        );
        fs::write(output.join("harness.rs"), "tampered").unwrap();
        assert!(verify(&output, &saved).is_err());
    }

    #[test]
    fn traversal_is_rejected_and_existing_archive_is_preserved() {
        let temp = Temp::new();
        let output = temp.0.join("archive");
        assert!(snapshot(&temp.0, &output, &[PathBuf::from("../private")]).is_err());
        fs::write(output.join("sentinel"), "preserved").unwrap();
        assert!(snapshot(&temp.0, &output, &[]).is_err());
        assert_eq!(
            fs::read_to_string(output.join("sentinel")).unwrap(),
            "preserved"
        );
    }
}
