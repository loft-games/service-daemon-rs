//! Incremental archive scheduler and durable provenance for recompressed output.
use super::{
    file_config::FileLogConfig,
    rotation::{active_date, pending_fragment, save_json, sync_directory},
};
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use flate2::{Compression, read::MultiGzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

const BLOCK: usize = 64 * 1024;
pub(super) const SCAN_INTERVAL: Duration = Duration::from_secs(60);

fn local_day_end(day: NaiveDate) -> Option<SystemTime> {
    let midnight = day.succ_opt()?.and_hms_opt(0, 0, 0)?;
    // Delegate offsets to the OS timezone, including gaps at local midnight.
    (0..=1440).find_map(|minutes| {
        Local
            .from_local_datetime(&midnight.checked_add_signed(chrono::Duration::minutes(minutes))?)
            .earliest()
            .map(Into::into)
    })
}

pub(super) fn rotated_log_name(prefix: &str, period: &str, sequence: Option<u32>) -> String {
    let (stem, extension) = split_extension(prefix);
    match sequence {
        Some(sequence) => format!("{stem}.{period}.{sequence}{extension}"),
        None => format!("{stem}.{period}{extension}"),
    }
}
fn split_extension(prefix: &str) -> (&str, &str) {
    match prefix.rfind('.') {
        Some(index) if index > 0 && index + 1 < prefix.len() => prefix.split_at(index),
        _ => (prefix, ""),
    }
}
fn component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn decode(value: &str) -> io::Result<String> {
    if !value.is_ascii() || !value.len().is_multiple_of(2) {
        return Err(io::Error::other("invalid legacy filename encoding"));
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&value[at..at + 2], 16).map_err(io::Error::other))
        .collect::<io::Result<Vec<_>>>()?;
    let name = String::from_utf8(bytes).map_err(io::Error::other)?;
    if !component(&name) {
        return Err(io::Error::other("invalid legacy filename"));
    }
    Ok(name)
}

fn salvage_strings(bytes: &[u8], key: &str) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let marker = format!("\"{key}\":");
    text.match_indices(&marker)
        .filter_map(|(position, _)| {
            serde_json::Deserializer::from_str(&text[position + marker.len()..])
                .into_iter::<String>()
                .next()
                .and_then(Result::ok)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Fragment,
    Daily,
    Monthly,
}
impl Stage {
    fn index(self) -> usize {
        match self {
            Self::Fragment => 0,
            Self::Daily => 1,
            Self::Monthly => 2,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Source {
    name: String,
    gzip: bool,
    length: u64,
    modified: SystemTime,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Cursor {
    source: usize,
    offset: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Output {
    name: String,
    sha256: String,
    length: u64,
    end: Cursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ArchiveWait {
    until: SystemTime,
    limit: Duration,
}
impl ArchiveWait {
    fn sample(limit: Duration) -> io::Result<Self> {
        let nanos = rand::random_range(0..=limit.as_nanos());
        let delay = Duration::new(
            (nanos / 1_000_000_000) as u64,
            (nanos % 1_000_000_000) as u32,
        );
        Ok(Self {
            until: SystemTime::now()
                .checked_add(delay)
                .ok_or_else(|| io::Error::other("archive jitter deadline overflow"))?,
            limit,
        })
    }
    fn deadline(&self) -> io::Result<Instant> {
        let remaining = self
            .until
            .duration_since(SystemTime::now())
            .unwrap_or_default();
        Instant::now()
            .checked_add(remaining.min(self.limit))
            .ok_or_else(|| io::Error::other("archive jitter exceeds the monotonic clock range"))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    id: String,
    stage: Stage,
    date: String,
    sources: Vec<Source>,
    outputs: Vec<Output>,
    cursor: Cursor,
    pending: Option<Output>,
    complete: bool,
    cleaned: bool,
    #[serde(default)]
    cleaned_sources: usize,
    level: u32,
    volume_size: u64,
    #[serde(default)]
    wait: Option<ArchiveWait>,
    #[serde(default)]
    volume_jitter: Duration,
}
impl Manifest {
    fn validate(&self) -> io::Result<()> {
        let valid_cursor = |cursor: Cursor| {
            cursor.source <= self.sources.len()
                && (cursor.source < self.sources.len() || cursor.offset == 0)
        };
        let mut previous = Cursor::default();
        let mut names = HashSet::new();
        let ranges_valid = self
            .outputs
            .iter()
            .chain(self.pending.iter())
            .all(|output| {
                let valid = valid_cursor(output.end)
                    && output.end >= previous
                    && names.insert(&output.name)
                    && !self.sources.iter().any(|source| source.name == output.name);
                previous = output.end;
                valid
            });
        let date_valid = match self.stage {
            Stage::Monthly => {
                NaiveDate::parse_from_str(&format!("{}-01", self.date), "%Y-%m-%d").is_ok()
            }
            _ => NaiveDate::parse_from_str(&self.date, "%Y-%m-%d").is_ok(),
        };
        if self.version != 2
            || !component(&self.id)
            || self.level > 9
            || self.volume_size == 0
            || Instant::now().checked_add(self.volume_jitter).is_none()
            || SystemTime::now().checked_add(self.volume_jitter).is_none()
            || self.wait.as_ref().is_some_and(|wait| {
                Instant::now().checked_add(wait.limit).is_none()
                    || SystemTime::now().checked_add(wait.limit).is_none()
            })
            || self.sources.is_empty()
            || !valid_cursor(self.cursor)
            || !ranges_valid
            || !date_valid
            || self
                .outputs
                .last()
                .map_or(Cursor::default(), |output| output.end)
                != self.cursor
            || self.cleaned_sources > self.cursor.source
            || (self.complete && self.pending.is_some())
            || (self.complete && self.cursor.source != self.sources.len())
            || (self.cleaned && (!self.complete || self.cleaned_sources != self.sources.len()))
            || self
                .sources
                .iter()
                .map(|source| &source.name)
                .collect::<HashSet<_>>()
                .len()
                != self.sources.len()
            || self.sources.iter().any(|source| !component(&source.name))
            || self
                .outputs
                .iter()
                .chain(self.pending.iter())
                .any(|output| {
                    !component(&output.name)
                        || output.sha256.len() != 64
                        || !output.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                        || output.end.source > self.sources.len()
                })
        {
            return Err(io::Error::other("invalid archive manifest"));
        }
        Ok(())
    }
}

struct HashedWriter {
    file: File,
    hash: Sha256,
    bytes: u64,
}
impl Write for HashedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let written = self.file.write(data)?;
        self.hash.update(&data[..written]);
        self.bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
struct Reader {
    stream: Box<dyn BufRead + Send>,
    skip: u64,
}
struct Verify {
    next: usize,
    file: Option<File>,
    hash: Sha256,
    bytes: u64,
}
struct Job {
    manifest: Manifest,
    path: PathBuf,
    directory: PathBuf,
    cursor: Cursor,
    reader: Option<Reader>,
    encoder: Option<GzEncoder<HashedWriter>>,
    verify: Option<Verify>,
    cleanup: usize,
    reserved: HashSet<String>,
    wake: Instant,
    waiting: bool,
}
impl Job {
    fn load(directory: &Path, path: PathBuf) -> io::Result<Self> {
        let manifest: Manifest = serde_json::from_slice(&fs::read(&path)?)?;
        manifest.validate()?;
        if path.file_stem().and_then(|name| name.to_str()) != Some(&manifest.id) {
            return Err(io::Error::other(
                "archive manifest identity differs from filename",
            ));
        }
        let cursor = manifest.cursor;
        let cleanup = manifest.cleaned_sources;
        let wake = manifest
            .wait
            .as_ref()
            .map(ArchiveWait::deadline)
            .transpose()?
            .unwrap_or_else(Instant::now);
        let mut reserved = HashSet::new();
        for entry in fs::read_dir(
            path.parent()
                .ok_or_else(|| io::Error::other("missing transaction directory"))?,
        )? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let bytes = fs::read(entry.path())?;
                if let Ok(other) = serde_json::from_slice::<Manifest>(&bytes) {
                    reserved.extend(
                        other
                            .outputs
                            .iter()
                            .chain(other.pending.iter())
                            .map(|output| output.name.clone()),
                    );
                    reserved.extend(other.sources.iter().map(|source| source.name.clone()));
                } else {
                    reserved.extend(
                        salvage_strings(&bytes, "name")
                            .into_iter()
                            .filter(|name| component(name)),
                    );
                }
            }
        }

        Ok(Self {
            manifest,
            path,
            directory: directory.into(),
            cursor,
            reader: None,
            encoder: None,
            verify: Some(Verify {
                next: 0,
                file: None,
                hash: Sha256::new(),
                bytes: 0,
            }),
            cleanup,
            reserved,
            wake,
            waiting: false,
        })
    }
    fn temporary(&self) -> PathBuf {
        self.path.with_extension("gz.tmp")
    }
    fn persist(&self) -> io::Result<()> {
        save_json(&self.path, &self.manifest)
    }
    fn verify_step(&mut self) -> io::Result<bool> {
        let Some(verify) = self.verify.as_mut() else {
            return Ok(false);
        };
        let output = self.manifest.outputs.get(verify.next).or(
            if verify.next == self.manifest.outputs.len() {
                self.manifest.pending.as_ref()
            } else {
                None
            },
        );
        let Some(output) = output else {
            self.verify = None;
            return Ok(false);
        };
        if verify.file.is_none() {
            let final_path = self.directory.join(&output.name);
            let path = if final_path.exists() {
                final_path
            } else if verify.next == self.manifest.outputs.len() {
                self.path.with_extension("gz.tmp")
            } else {
                final_path
            };
            verify.file = Some(File::open(path)?);
        }
        let mut buffer = [0; BLOCK];
        let count = verify
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("verification file missing"))?
            .read(&mut buffer)?;
        if count > 0 {
            verify.hash.update(&buffer[..count]);
            verify.bytes += count as u64;
            return Ok(true);
        }
        if verify.bytes != output.length || hex(&verify.hash.clone().finalize()) != output.sha256 {
            return Err(io::Error::other(format!(
                "archive content mismatch: {}",
                output.name
            )));
        }
        verify.file = None;
        verify.hash = Sha256::new();
        verify.bytes = 0;
        verify.next += 1;
        Ok(true)
    }
    fn recover_pending(&mut self) -> io::Result<()> {
        if let Some(output) = self.manifest.pending.take() {
            let target = self.directory.join(&output.name);
            if !target.exists() {
                fs::rename(self.temporary(), &target)?;
                sync_directory(&self.directory)?;
            }
            self.manifest.cursor = output.end;
            self.cursor = output.end;
            self.manifest.outputs.push(output);
            self.persist()?;
        }
        Ok(())
    }
    fn output_name(&self) -> io::Result<String> {
        let mut sequence = 1_u32;
        loop {
            let marker = match self.manifest.stage {
                Stage::Fragment => {
                    let name = format!("{}.gz", self.manifest.sources[0].name);
                    if self.directory.join(&name).exists() {
                        return Err(io::Error::other(
                            "existing fragment archive has no matching transaction proof",
                        ));
                    }
                    return Ok(name);
                }
                Stage::Daily => format!("daily.{}.{sequence}", self.manifest.date),
                Stage::Monthly => format!("monthly.{}.part{sequence:04}", self.manifest.date),
            };
            let prefix = self
                .path
                .parent()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other("archive namespace missing"))?;
            let prefix = decode(prefix)?;
            let name = format!("{}.gz", rotated_log_name(&prefix, &marker, None));
            if !self.directory.join(&name).exists()
                && !self.reserved.contains(&name)
                && !self
                    .manifest
                    .outputs
                    .iter()
                    .any(|output| output.name == name)
            {
                return Ok(name);
            }
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| io::Error::other("archive sequence exhausted"))?;
        }
    }
    fn ensure_encoder(&mut self) -> io::Result<()> {
        if self.encoder.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(self.temporary())?;
            self.encoder = Some(GzEncoder::new(
                HashedWriter {
                    file,
                    hash: Sha256::new(),
                    bytes: 0,
                },
                Compression::new(self.manifest.level),
            ));
        }
        Ok(())
    }
    fn seal(&mut self) -> io::Result<()> {
        let Some(encoder) = self.encoder.take() else {
            return Ok(());
        };
        let output = encoder.finish()?;
        output.file.sync_all()?;
        let entry = Output {
            name: self.output_name()?,
            sha256: hex(&output.hash.finalize()),
            length: output.bytes,
            end: self.cursor,
        };
        drop(output.file);
        if self.manifest.stage == Stage::Monthly {
            let wait = ArchiveWait::sample(self.manifest.volume_jitter)?;
            self.wake = wait.deadline()?;
            self.manifest.wait = Some(wait);
        }
        self.manifest.pending = Some(entry);
        self.persist()?;
        self.recover_pending()
    }
    /// Performs one bounded read/encode/verification block or one cleanup action.
    fn step(&mut self) -> io::Result<bool> {
        if self.verify_step()? {
            return Ok(false);
        }
        self.recover_pending()?;
        if self.cleanup < self.manifest.cursor.source {
            let source = &self.manifest.sources[self.cleanup];
            match fs::metadata(self.directory.join(&source.name)) {
                Ok(meta) => {
                    if meta.len() != source.length || meta.modified()? != source.modified {
                        return Err(io::Error::other("archive input changed before cleanup"));
                    }
                    fs::remove_file(self.directory.join(&source.name))?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let marker = self.directory.join(format!("{}.needs-repack", source.name));
            match fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            self.cleanup += 1;
            self.manifest.cleaned_sources = self.cleanup;
            sync_directory(&self.directory)?;
            self.persist()?;
            return Ok(false);
        }
        if self.manifest.complete {
            sync_directory(&self.directory)?;
            self.manifest.cleaned = true;
            self.persist()?;
            return Ok(true);
        }
        if self.cursor.source == self.manifest.sources.len() {
            if self.manifest.outputs.is_empty() {
                self.ensure_encoder()?;
            }
            self.seal()?;
            self.manifest.complete = true;
            self.persist()?;
            return Ok(false);
        }
        if self.reader.is_none() {
            let source = &self.manifest.sources[self.cursor.source];
            let file = File::open(self.directory.join(&source.name))?;
            let meta = file.metadata()?;
            if meta.len() != source.length || meta.modified()? != source.modified {
                return Err(io::Error::other("archive input identity changed"));
            }
            let stream: Box<dyn BufRead + Send> = if source.gzip {
                Box::new(BufReader::with_capacity(
                    BLOCK,
                    MultiGzDecoder::new(BufReader::new(file)),
                ))
            } else {
                Box::new(BufReader::with_capacity(BLOCK, file))
            };
            self.reader = Some(Reader {
                stream,
                skip: self.cursor.offset,
            });
        }
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| io::Error::other("archive input missing"))?;
        let data = reader.stream.fill_buf()?;
        if data.is_empty() {
            if reader.skip > 0 {
                return Err(io::Error::other("archive resume offset past end of source"));
            }
            self.cursor.source += 1;
            self.cursor.offset = 0;
            self.reader = None;
            // The last published volume can end exactly at this EOF. Advance
            // its proven range across EOF (and empty sources) without emitting
            // an empty additional volume or leaving a covered input behind.
            if self.encoder.is_none()
                && let Some(output) = self.manifest.outputs.last_mut()
            {
                output.end = self.cursor;
                self.manifest.cursor = self.cursor;
                self.persist()?;
            }
            return Ok(false);
        }
        if reader.skip > 0 {
            let count = (data.len().min(BLOCK) as u64).min(reader.skip) as usize;
            reader.skip -= count as u64;
            reader.stream.consume(count);
            return Ok(false);
        }
        // Release the input borrow while creating the output stream.
        if self.encoder.is_none() && Instant::now() < self.wake {
            self.waiting = true;
            return Ok(false);
        }
        self.ensure_encoder()?;
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| io::Error::other("archive input missing"))?;
        let data = reader.stream.fill_buf()?;
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| io::Error::other("archive encoder missing"))?;
        let split = self.manifest.stage == Stage::Monthly
            && encoder.get_ref().bytes >= self.manifest.volume_size;
        let count = if split {
            data[..data.len().min(BLOCK)]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(data.len().min(BLOCK), |at| at + 1)
        } else {
            data.len().min(BLOCK)
        };
        let seal = split && data[count - 1] == b'\n';
        encoder.write_all(&data[..count])?;
        reader.stream.consume(count);
        self.cursor.offset += count as u64;
        if seal {
            self.seal()?;
        }
        Ok(false)
    }
}

#[derive(Clone)]
struct Candidate {
    name: String,
    day: NaiveDate,
    stage: Stage,
    gzip: bool,
    order: (String, u64),
}

pub(super) struct ArchiveStore {
    config: FileLogConfig,
    directory: PathBuf,
    transactions: PathBuf,
    jobs: [Option<Job>; 3],
    legacy_job: Option<legacy::LegacyJob>,
    retry: HashMap<PathBuf, Instant>,
    refresh: Instant,
    scheduled: HashMap<PathBuf, Instant>,
    paused: Vec<Job>,
}
impl ArchiveStore {
    pub(super) fn new(config: &FileLogConfig) -> io::Result<Self> {
        let directory = PathBuf::from(&config.directory);
        let transactions = directory
            .join(".service-daemon-archive-v2")
            .join(hex(config.file_prefix.as_bytes()));
        fs::create_dir_all(&transactions)?;
        Ok(Self {
            config: config.clone(),
            directory,
            transactions,
            jobs: [None, None, None],
            legacy_job: None,
            retry: HashMap::new(),
            refresh: Instant::now(),
            scheduled: HashMap::new(),
            paused: Vec::new(),
        })
    }
    pub(super) fn request(&mut self) {
        self.refresh = Instant::now();
    }
    fn start_jitter(&self, stage: Stage) -> Duration {
        match stage {
            Stage::Fragment => Duration::ZERO,
            Stage::Daily => self.config.archive.daily_start_jitter,
            Stage::Monthly => self.config.archive.monthly_start_jitter,
        }
    }
    fn parse(&self, name: &str) -> Option<Candidate> {
        let gzip = name.ends_with(".gz");
        let bare = name.strip_suffix(".gz").unwrap_or(name);
        let (stem, extension) = split_extension(&self.config.file_prefix);
        let suffix = bare
            .strip_prefix(stem)?
            .strip_prefix('.')?
            .strip_suffix(extension);
        let suffix =
            suffix.or_else(|| bare.strip_prefix(&format!("{}.", self.config.file_prefix)))?;
        if suffix.starts_with("monthly.") {
            return None;
        }
        let (stage, period, segment, daily) = if let Some(period) = suffix.strip_prefix("daily.") {
            (Stage::Monthly, period, false, true)
        } else if let Some(period) = suffix.strip_prefix("segment.") {
            (
                if gzip { Stage::Daily } else { Stage::Fragment },
                period,
                true,
                false,
            )
        } else {
            (
                if gzip { Stage::Daily } else { Stage::Fragment },
                suffix,
                false,
                false,
            )
        };
        let day = NaiveDate::parse_from_str(period.get(..10)?, "%Y-%m-%d").ok()?;
        let period = period.strip_suffix(".merged").unwrap_or(period);
        let (order, sequence) = match period.rsplit_once('.') {
            Some((head, value)) => (head, value.parse::<u64>().ok().filter(|value| *value > 0)?),
            None => (period, 0),
        };
        if segment {
            chrono::NaiveDateTime::parse_from_str(order, "%Y-%m-%d-%H-%M-%S").ok()?;
            if sequence == 0 {
                return None;
            }
        } else if daily {
            if order.len() != 10 || sequence == 0 || !gzip {
                return None;
            }
        } else if order.len() != 10 {
            chrono::NaiveDateTime::parse_from_str(&format!("{order}:00"), "%Y-%m-%d-%H:%M").ok()?;
        }
        let order = order.to_owned();
        Some(Candidate {
            name: name.into(),
            day,
            stage,
            gzip,
            order: (order, sequence),
        })
    }
    fn create(&self, stage: Stage, date: String, candidates: &[Candidate]) -> io::Result<PathBuf> {
        let sources = candidates
            .iter()
            .map(|item| {
                let meta = fs::metadata(self.directory.join(&item.name))?;
                Ok(Source {
                    name: item.name.clone(),
                    gzip: item.gzip,
                    length: meta.len(),
                    modified: meta.modified()?,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let id = uuid::Uuid::now_v7().to_string();
        let path = self.transactions.join(format!("{id}.json"));
        let level = match stage {
            Stage::Fragment => self.config.archive.fragment_compression_level,
            Stage::Daily => self.config.archive.daily_compression_level,
            Stage::Monthly => self.config.archive.monthly_compression_level,
        };
        let manifest = Manifest {
            version: 2,
            id,
            stage,
            date,
            sources,
            outputs: Vec::new(),
            cursor: Cursor::default(),
            pending: None,
            complete: false,
            cleaned: false,
            cleaned_sources: 0,
            level,
            volume_size: self.config.archive.monthly_volume_size,
            wait: if stage == Stage::Fragment {
                None
            } else {
                Some(ArchiveWait::sample(self.start_jitter(stage))?)
            },
            volume_jitter: self.config.archive.monthly_volume_jitter,
        };
        save_json(&path, &manifest)?;
        Ok(path)
    }
    fn discover(&mut self, now: SystemTime) -> io::Result<()> {
        let (mut blocked, legacy_paths) =
            legacy::discover(&self.directory, &self.config.file_prefix)?;
        if self.legacy_job.is_none() {
            for path in legacy_paths {
                if self
                    .retry
                    .get(&path)
                    .is_some_and(|retry| Instant::now() < *retry)
                {
                    continue;
                }
                match legacy::LegacyJob::load(
                    &self.directory,
                    &self.config.file_prefix,
                    path.clone(),
                ) {
                    Ok(job) => {
                        self.legacy_job = Some(job);
                        break;
                    }
                    Err(error) => {
                        eprintln!(
                            "service-daemon legacy transaction {} retained: {error}",
                            path.display()
                        );
                        self.retry
                            .insert(path, Instant::now() + Duration::from_secs(5));
                    }
                }
            }
        }
        blocked.extend(pending_fragment(&self.config)?);
        let mut claimed = HashSet::new();
        let mut hidden = HashSet::new();
        let mut manifests = Vec::new();
        let mut quarantined = HashSet::<String>::new();
        for entry in fs::read_dir(&self.transactions)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let bytes = fs::read(entry.path())?;
            match serde_json::from_slice::<Manifest>(&bytes)
                .map_err(io::Error::other)
                .and_then(|manifest| {
                    manifest.validate()?;
                    if entry.path().file_stem().and_then(|name| name.to_str()) != Some(&manifest.id)
                    {
                        return Err(io::Error::other(
                            "archive manifest identity differs from filename",
                        ));
                    }
                    Ok(manifest)
                }) {
                Ok(manifest) => {
                    claimed.extend(manifest.sources.iter().map(|source| source.name.clone()));
                    if !manifest.cleaned {
                        hidden.extend(
                            manifest
                                .outputs
                                .iter()
                                .chain(manifest.pending.iter())
                                .map(|output| output.name.clone()),
                        );
                    }
                    manifests.push((entry.path(), manifest));
                }
                Err(error) => {
                    blocked.extend(
                        salvage_strings(&bytes, "name")
                            .into_iter()
                            .filter(|name| component(name)),
                    );
                    let dates = salvage_strings(&bytes, "date");
                    if dates.is_empty() {
                        return Err(io::Error::other(format!(
                            "cannot identify damaged transaction ownership: {}: {error}",
                            entry.path().display()
                        )));
                    }
                    quarantined.extend(dates);
                    eprintln!(
                        "service-daemon damaged archive transaction retained: {}: {error}",
                        entry.path().display()
                    );
                }
            }
        }
        for (path, manifest) in &mut manifests {
            let index = manifest.stage.index();
            if self
                .jobs
                .iter()
                .flatten()
                .chain(self.paused.iter())
                .any(|job| job.path == *path)
            {
                continue;
            }
            if !manifest.cleaned && !manifest.complete && manifest.stage != Stage::Fragment {
                if manifest.wait.is_none() {
                    // Adopt pre-jitter v2 transactions once, including startup backlog.
                    manifest.wait = Some(ArchiveWait::sample(self.start_jitter(manifest.stage))?);
                    manifest.volume_jitter = self.config.archive.monthly_volume_jitter;
                    save_json(path, manifest)?;
                }
                if let Some(wait) = &manifest.wait {
                    let deadline = if let Some(deadline) = self.scheduled.get(path) {
                        *deadline
                    } else {
                        let deadline = wait.deadline()?;
                        self.scheduled.insert(path.clone(), deadline);
                        deadline
                    };
                    if Instant::now() < deadline {
                        self.refresh = self.refresh.min(deadline);
                        continue;
                    }
                }
            }
            if !manifest.cleaned
                && self.jobs[index].is_none()
                && self
                    .retry
                    .get(path)
                    .is_none_or(|retry| Instant::now() >= *retry)
            {
                let mut job = Job::load(&self.directory, path.clone())?;
                if let Some(deadline) = self.scheduled.remove(path) {
                    job.wake = deadline;
                }
                self.jobs[index] = Some(job);
            }
        }
        let active_day = active_date(&self.config)?
            .and_then(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok());
        let mut candidates = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if claimed.contains(&name) || hidden.contains(&name) || blocked.contains(&name) {
                continue;
            }
            if let Some(candidate) = self.parse(&name) {
                if quarantined
                    .iter()
                    .any(|date| candidate.day.to_string().starts_with(date))
                {
                    continue;
                }
                candidates.push(candidate);
            }
        }
        candidates.sort_by_key(|item| (item.day, item.order.clone()));
        if self.jobs[0].is_none()
            && let Some(item) = candidates.iter().find(|item| {
                item.stage == Stage::Fragment
                    && pending_fragment(&self.config)
                        .is_ok_and(|pending| pending.as_deref() != Some(&item.name))
            })
        {
            let path = self.create(
                Stage::Fragment,
                item.day.to_string(),
                std::slice::from_ref(item),
            )?;
            self.jobs[0] = Some(Job::load(&self.directory, path)?);
        }
        let local: DateTime<Local> = now.into();
        let mut days = BTreeMap::<NaiveDate, Vec<Candidate>>::new();
        for item in &candidates {
            if item.stage == Stage::Daily {
                days.entry(item.day).or_default().push(item.clone());
            }
        }
        if self.jobs[1].is_none() {
            for (day, group) in days {
                let Some(end) = local_day_end(day) else {
                    continue;
                };
                let ready = now
                    .duration_since(end)
                    .is_ok_and(|age| age >= self.config.archive.daily_archive_delay);
                if !ready
                    || quarantined
                        .iter()
                        .any(|date| day.to_string().starts_with(date))
                    || active_day == Some(day)
                    || blocked
                        .iter()
                        .filter_map(|name| self.parse(name))
                        .any(|item| item.day == day)
                    || candidates
                        .iter()
                        .any(|item| item.day == day && item.stage == Stage::Fragment)
                    || manifests.iter().any(|(_, manifest)| {
                        !manifest.cleaned
                            && manifest.stage <= Stage::Daily
                            && manifest.date == day.to_string()
                    })
                {
                    continue;
                }
                let path = self.create(Stage::Daily, day.to_string(), &group)?;
                if self.start_jitter(Stage::Daily).is_zero() {
                    self.jobs[1] = Some(Job::load(&self.directory, path)?);
                } else {
                    self.request();
                }
                break;
            }
        }
        let mut months = BTreeMap::<String, Vec<Candidate>>::new();
        for item in &candidates {
            if item.stage == Stage::Monthly {
                months
                    .entry(item.day.format("%Y-%m").to_string())
                    .or_default()
                    .push(item.clone());
            }
        }
        if self.jobs[2].is_none() {
            for (month, group) in months {
                if quarantined
                    .iter()
                    .any(|date| date.starts_with(&month) || month.starts_with(date))
                    || blocked
                        .iter()
                        .filter_map(|name| self.parse(name))
                        .any(|item| item.day.format("%Y-%m").to_string() == month)
                    || month >= local.format("%Y-%m").to_string()
                    || active_day.is_some_and(|day| day.format("%Y-%m").to_string() == month)
                    || candidates.iter().any(|item| {
                        item.day.format("%Y-%m").to_string() == month
                            && item.stage != Stage::Monthly
                    })
                    || manifests
                        .iter()
                        .any(|(_, manifest)| !manifest.cleaned && manifest.date.starts_with(&month))
                {
                    continue;
                }
                let path = self.create(Stage::Monthly, month, &group)?;
                if self.start_jitter(Stage::Monthly).is_zero() {
                    self.jobs[2] = Some(Job::load(&self.directory, path)?);
                } else {
                    self.request();
                }
                break;
            }
        }
        Ok(())
    }
    /// Returns whether work remains. One call processes at most one data block.
    pub(super) fn step(&mut self, now: SystemTime, cancel: &AtomicBool) -> io::Result<bool> {
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        // Park a waiting monthly reader without blocking another month or stage.
        if let Some(index) = self.paused.iter().position(|job| {
            Instant::now() >= job.wake && self.jobs[job.manifest.stage.index()].is_none()
        }) {
            let mut job = self.paused.swap_remove(index);
            job.waiting = false;
            let stage = job.manifest.stage.index();
            self.jobs[stage] = Some(job);
        }
        for job in &self.paused {
            if job.wake > Instant::now() {
                self.refresh = self.refresh.min(job.wake);
            }
        }
        if Instant::now() >= self.refresh {
            self.refresh = Instant::now() + SCAN_INTERVAL;
            self.discover(now)?;
            if let Some(retry) = self
                .retry
                .values()
                .filter(|retry| **retry > Instant::now())
                .min()
            {
                self.refresh = self.refresh.min(*retry);
            }
        }
        if self.jobs[0].is_none()
            && let Some(job) = self.legacy_job.as_mut()
        {
            match job.step() {
                Ok(true) => {
                    self.legacy_job = None;
                    self.request();
                }
                Ok(false) => {}
                Err(error) => {
                    let path = job.path.clone();
                    self.retry
                        .insert(path.clone(), Instant::now() + Duration::from_secs(5));
                    self.legacy_job = None;
                    self.request();
                    return Err(io::Error::other(format!(
                        "legacy archive transaction {}: {error}",
                        path.display()
                    )));
                }
            }
            return Ok(true);
        }
        let Some(index) = self.jobs.iter().position(Option::is_some) else {
            return Ok(self.refresh <= Instant::now());
        };
        let job = self.jobs[index]
            .as_mut()
            .ok_or_else(|| io::Error::other("archive job missing"))?;
        match job.step() {
            Ok(true) => {
                self.jobs[index] = None;
                self.request();
            }
            Ok(false) if job.waiting => {
                if let Some(job) = self.jobs[index].take() {
                    self.paused.push(job);
                }
                self.request();
            }
            Ok(false) => {}
            Err(error) => {
                let path = job.path.clone();
                self.retry
                    .insert(path.clone(), Instant::now() + Duration::from_secs(5));
                self.jobs[index] = None;
                self.request();
                return Err(io::Error::other(format!(
                    "archive transaction {}: {error}",
                    path.display()
                )));
            }
        }
        Ok(true)
    }
}

mod legacy;

#[cfg(test)]
fn recover_legacy(directory: &Path, prefix: &str) -> io::Result<HashSet<String>> {
    let (blocked, paths) = legacy::discover(directory, prefix)?;
    for path in paths {
        if let Ok(mut job) = legacy::LegacyJob::load(directory, prefix, path) {
            while !job.step()? {}
        }
    }
    Ok(blocked)
}

#[cfg(test)]
mod tests;
