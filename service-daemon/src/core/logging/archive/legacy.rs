//! Resumable compatibility with the former concatenation transaction format.
use super::*;
use std::io::{Seek, SeekFrom};

type Decoder = MultiGzDecoder<BufReader<File>>;

enum Phase {
    Restore,
    Compare,
    Validate,
    Stage,
    Delete,
    Finish,
}

pub(super) struct LegacyJob {
    pub(super) path: PathBuf,
    directory: PathBuf,
    staging: PathBuf,
    temporary: PathBuf,
    sources: Vec<String>,
    target: String,
    published: bool,
    candidate_temporary: bool,
    phase: Phase,
    index: usize,
    decoder: Option<Decoder>,
    restore_missing: bool,
    target_file: Option<File>,
    source_file: Option<File>,
}

/// Metadata-only discovery. Every unresolved manifest keeps ownership of its
/// inputs, including malformed manifests for which names can still be recovered.
pub(super) fn discover(
    directory: &Path,
    prefix: &str,
) -> io::Result<(HashSet<String>, Vec<PathBuf>)> {
    let root = directory.join(".service-daemon-archive-transactions");
    let mut blocked = HashSet::new();
    let mut paths = Vec::new();
    if !root.exists() {
        return Ok((blocked, paths));
    }
    let namespace = format!("{}-", hex(prefix.as_bytes()));
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry.file_name().to_string_lossy().starts_with(&namespace)
        {
            continue;
        }
        for entry in fs::read_dir(entry.path())? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "manifest")
            {
                continue;
            }
            let bytes = fs::read(entry.path())?;
            let text = String::from_utf8_lossy(&bytes);
            blocked.extend(
                text.lines()
                    .filter_map(|line| {
                        line.strip_prefix("source=")
                            .or_else(|| line.strip_prefix("target="))
                    })
                    .filter_map(|name| decode(name).ok()),
            );
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok((blocked, paths))
}

impl LegacyJob {
    pub(super) fn load(directory: &Path, prefix: &str, path: PathBuf) -> io::Result<Self> {
        let text = fs::read_to_string(&path)?;
        let sources: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("source="))
            .map(decode)
            .collect::<io::Result<_>>()?;
        let value = |key: &str| text.lines().find_map(|line| line.strip_prefix(key));
        let target = value("target=")
            .map(decode)
            .transpose()?
            .ok_or_else(|| io::Error::other("legacy target missing"))?;
        let id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("legacy transaction id missing"))?;
        let staging = value("staging=").map(decode).transpose()?;
        let temporary = value("temporary=").map(decode).transpose()?;
        let count = value("source-count=").and_then(|count| count.parse::<usize>().ok());
        let (stem, _) = split_extension(prefix);
        if !text.ends_with("\nend\n")
            || !(text.starts_with("phase=prepared\n") || text.starts_with("phase=published\n"))
            || sources.is_empty()
            || count != Some(sources.len())
            || staging.as_deref() != Some(&format!("{id}.sources"))
            || temporary.as_deref() != Some(&format!("{id}.tmp"))
            || sources.iter().any(|name| {
                !name.starts_with(&format!("{stem}.")) || !name.ends_with(".gz") || name == &target
            })
            || sources.iter().collect::<HashSet<_>>().len() != sources.len()
            || !target.starts_with(&format!("{stem}."))
            || !target.contains(".merged")
            || !target.ends_with(".gz")
        {
            return Err(io::Error::other(
                "invalid legacy transaction; inputs retained",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("legacy transaction parent missing"))?;
        Ok(Self {
            directory: directory.into(),
            staging: parent.join(format!("{id}.sources")),
            temporary: parent.join(format!("{id}.tmp")),
            published: text.starts_with("phase=published\n"),
            candidate_temporary: false,
            path,
            sources,
            target,
            phase: Phase::Restore,
            index: 0,
            decoder: None,
            restore_missing: false,
            target_file: None,
            source_file: None,
        })
    }
    fn transition(&mut self, phase: Phase) {
        self.phase = phase;
        self.index = 0;
        self.decoder = None;
        self.source_file = None;
        self.target_file = None;
    }
    fn uncommitted(&mut self) -> io::Result<bool> {
        if self.candidate_temporary {
            return Err(io::Error::other(
                "unpublished legacy output cannot be proven; temporary and sources retained",
            ));
        }
        if self.published {
            return Err(io::Error::other(
                "published legacy target cannot be proven; inputs retained",
            ));
        }
        self.transition(Phase::Finish);
        Ok(false)
    }
    /// One 64KiB data block or one metadata operation, so cancellation and new
    /// fragment work are checked even during migration of very large archives.
    pub(super) fn step(&mut self) -> io::Result<bool> {
        match self.phase {
            Phase::Restore => {
                if self.index == self.sources.len() {
                    self.transition(Phase::Compare);
                    return Ok(false);
                }
                let source = self.directory.join(&self.sources[self.index]);
                let staged = self.staging.join(format!("{}.source", self.index));
                if self.decoder.is_none() {
                    self.restore_missing = !source.is_file();
                    self.decoder = Some(MultiGzDecoder::new(BufReader::new(File::open(
                        if self.restore_missing {
                            &staged
                        } else {
                            &source
                        },
                    )?)));
                }
                if self
                    .decoder
                    .as_mut()
                    .ok_or_else(|| io::Error::other("legacy decoder missing"))?
                    .read(&mut [0; BLOCK])?
                    > 0
                {
                    return Ok(false);
                }
                self.decoder = None;
                if self.restore_missing {
                    fs::hard_link(&staged, source)?;
                    sync_directory(&self.directory)?;
                }
                self.index += 1;
            }
            Phase::Compare => {
                if self.target_file.is_none() {
                    let target_path = self.directory.join(&self.target);
                    self.candidate_temporary = !target_path.is_file() && self.temporary.is_file();
                    if !target_path.is_file() && !self.candidate_temporary {
                        return self.uncommitted();
                    }
                    let total = self.sources.iter().try_fold(0_u64, |total, name| {
                        total
                            .checked_add(fs::metadata(self.directory.join(name))?.len())
                            .ok_or_else(|| io::Error::other("legacy source length overflow"))
                    })?;
                    let mut file = File::open(if self.candidate_temporary {
                        self.temporary.clone()
                    } else {
                        target_path
                    })?;
                    let length = file.metadata()?.len();
                    if length < total {
                        return self.uncommitted();
                    }
                    file.seek(SeekFrom::Start(length - total))?;
                    self.target_file = Some(file);
                }
                if self.index == self.sources.len() {
                    self.transition(Phase::Validate);
                    return Ok(false);
                }
                if self.source_file.is_none() {
                    self.source_file =
                        Some(File::open(self.directory.join(&self.sources[self.index]))?);
                }
                let mut left = [0; BLOCK];
                let mut right = [0; BLOCK];
                let count = self
                    .source_file
                    .as_mut()
                    .ok_or_else(|| io::Error::other("legacy source missing"))?
                    .read(&mut left)?;
                if count == 0 {
                    self.index += 1;
                    self.source_file = None;
                    return Ok(false);
                }
                self.target_file
                    .as_mut()
                    .ok_or_else(|| io::Error::other("legacy target missing"))?
                    .read_exact(&mut right[..count])?;
                if left[..count] != right[..count] {
                    return self.uncommitted();
                }
            }
            Phase::Validate => {
                if self.decoder.is_none() {
                    self.decoder = Some(MultiGzDecoder::new(BufReader::new(File::open(
                        if self.candidate_temporary {
                            self.temporary.clone()
                        } else {
                            self.directory.join(&self.target)
                        },
                    )?)));
                }
                if self
                    .decoder
                    .as_mut()
                    .ok_or_else(|| io::Error::other("legacy decoder missing"))?
                    .read(&mut [0; BLOCK])?
                    == 0
                {
                    self.decoder = None;
                    if self.candidate_temporary {
                        OpenOptions::new()
                            .write(true)
                            .open(&self.temporary)?
                            .sync_all()?;
                        let target = self.directory.join(&self.target);
                        if target.exists() {
                            return Err(io::Error::other(
                                "legacy publication target already exists",
                            ));
                        }
                        fs::rename(&self.temporary, target)?;
                        sync_directory(&self.directory)?;
                    }
                    self.transition(Phase::Stage);
                }
            }
            Phase::Stage => {
                if self.index == self.sources.len() {
                    sync_directory(&self.staging)?;
                    self.transition(Phase::Delete);
                    return Ok(false);
                }
                fs::create_dir_all(&self.staging)?;
                let staged = self.staging.join(format!("{}.source", self.index));
                let temporary = staged.with_extension("source.new");
                match fs::remove_file(&temporary) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                // Refresh all backups from the proven current sources before
                // any deletion, including an older pre-existing staging copy.
                fs::hard_link(self.directory.join(&self.sources[self.index]), &temporary)?;
                fs::rename(temporary, staged)?;
                self.index += 1;
            }
            Phase::Delete => {
                if self.index == self.sources.len() {
                    sync_directory(&self.directory)?;
                    self.transition(Phase::Finish);
                    return Ok(false);
                }
                match fs::remove_file(self.directory.join(&self.sources[self.index])) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                self.index += 1;
            }
            Phase::Finish => {
                // Remove the manifest first: no remaining input cleanup relies
                // on backups once recovery/import is ready. Stale backups are
                // harmless if a crash interrupts their subsequent removal.
                fs::remove_file(&self.path)?;
                sync_directory(
                    self.path
                        .parent()
                        .ok_or_else(|| io::Error::other("legacy parent missing"))?,
                )?;
                if self.staging.exists() {
                    fs::remove_dir_all(&self.staging)?;
                }
                match fs::remove_file(&self.temporary) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                return Ok(true);
            }
        }
        Ok(false)
    }
}
