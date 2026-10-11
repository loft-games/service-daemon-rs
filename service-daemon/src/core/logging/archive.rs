//! Background file-log archiving.
//!
//! This module deliberately contains no executor or worker thread.  The file
//! logging service owns the scheduling and calls [`ArchiveStore::maintain`]
//! after a rotation (and during shutdown).  Keeping the filesystem state
//! machine here makes retries safe when a process is stopped between the
//! publication of an archive and removal of its source files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Timelike, Utc};
use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;

use super::file::RotationPolicy;

const COMPACTION_MIN_FRAGMENTS: usize = 3;
const COMPACTION_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const REPACK_MIN_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const COPY_BUFFER_SIZE: usize = 64 * 1024;
const TRANSACTION_DIRECTORY: &str = ".service-daemon-archive-transactions";
const TRANSACTION_SUFFIX: &str = ".manifest";

/// Filesystem archive state for one rolling file appender.
pub(super) struct ArchiveStore {
    directory: PathBuf,
    prefix: String,
    rotation: RotationPolicy,
}

impl ArchiveStore {
    pub(super) fn new(directory: PathBuf, prefix: String, rotation: RotationPolicy) -> Self {
        Self {
            directory,
            prefix,
            rotation,
        }
    }

    /// Performs one bounded maintenance pass.
    ///
    /// Every operation is idempotent: an archive is published before its
    /// source is removed, and a retry recognizes an already-published suffix
    /// before appending anything again.  `cancel` is checked between files and
    /// copy chunks so shutdown can stop without deleting recoverable input.
    pub(super) fn maintain(
        &self,
        active: Option<&Path>,
        now: SystemTime,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if cancel.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        if !self.directory.exists() {
            return Ok(());
        }
        if !self.directory.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!(
                    "log archive path is not a directory: {}",
                    self.directory.display()
                ),
            ));
        }

        let mut maintenance_error = None;
        record_maintenance_result(
            &mut maintenance_error,
            "recovering transactions",
            self.recover_transactions(cancel),
            cancel,
        )?;
        record_maintenance_result(
            &mut maintenance_error,
            "cleaning temporary files",
            self.remove_stale_temporary_files(cancel),
            cancel,
        )?;

        let current_period = Period::from_system_time(self.rotation, now);
        let closed_before = active
            .and_then(|path| path.file_name()?.to_str())
            .and_then(|name| self.parse_plain_name(name))
            .map_or(current_period, |period| period.min(current_period));
        let mut plain = Vec::new();
        let mut fragments = Vec::new();

        for path in scan_files(&self.directory, &mut maintenance_error, cancel)? {
            if cancel.load(AtomicOrdering::Acquire) {
                return maintenance_error.map_or(Ok(()), Err);
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(period) = self.parse_plain_name(name) {
                if period < closed_before && !is_active(&path, active) {
                    plain.push((path, period));
                }
                continue;
            }
            if let Some(fragment) = self.parse_fragment_name(name)
                && fragment.period < closed_before
                && !is_active(&path, active)
            {
                fragments.push((path, fragment));
            }
        }

        plain.sort_by(|left, right| left.0.cmp(&right.0));
        for (path, _) in plain {
            if cancel.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            record_maintenance_result(
                &mut maintenance_error,
                format!("compressing {}", path.display()),
                self.compress_plain(&path, cancel),
                cancel,
            )?;
        }

        let mut groups: Vec<(NaiveDate, Vec<Fragment>)> = Vec::new();
        for (path, fragment) in fragments {
            let old_enough = match is_old_enough(&path, now, COMPACTION_MIN_AGE) {
                Ok(old_enough) => old_enough,
                Err(error) => {
                    record_maintenance_error(
                        &mut maintenance_error,
                        format!("checking fragment {}", path.display()),
                        error,
                    );
                    continue;
                }
            };
            if !old_enough {
                continue;
            }
            let day = fragment.period.date();
            let Some((_, group)) = groups.iter_mut().find(|(group_day, _)| *group_day == day)
            else {
                groups.push((day, Vec::new()));
                let index = groups.len() - 1;
                groups[index].1.push(Fragment { path });
                continue;
            };
            group.push(Fragment { path });
        }

        groups.sort_by_key(|left| left.0);
        for (day, mut group) in groups {
            if cancel.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            if group.len() < COMPACTION_MIN_FRAGMENTS {
                continue;
            }
            group.sort_by(|left, right| left.path.cmp(&right.path));
            record_maintenance_result(
                &mut maintenance_error,
                format!("compacting {day}"),
                self.compact(day, &group, cancel),
                cancel,
            )?;
        }

        record_maintenance_result(
            &mut maintenance_error,
            "repacking archives",
            self.repack(now, cancel),
            cancel,
        )?;

        maintenance_error.map_or(Ok(()), Err)
    }

    fn parse_plain_name(&self, name: &str) -> Option<Period> {
        let suffix = name.strip_prefix(&format!("{}.", self.prefix))?;
        if suffix.ends_with(".gz") || suffix.ends_with(".merged.gz") {
            return None;
        }
        parse_period(suffix, self.rotation)
    }

    fn parse_fragment_name(&self, name: &str) -> Option<FragmentName> {
        let suffix = name.strip_prefix(&format!("{}.", self.prefix))?;
        let suffix = suffix.strip_suffix(".gz")?;
        if suffix.ends_with(".merged") {
            return None;
        }
        let period = parse_period(suffix, self.rotation)?;
        Some(FragmentName { period })
    }

    fn parse_merged_name(&self, name: &str) -> Option<NaiveDate> {
        let suffix = name.strip_prefix(&format!("{}.", self.prefix))?;
        let suffix = suffix.strip_suffix(".merged.gz")?;
        NaiveDate::parse_from_str(suffix, "%Y-%m-%d").ok()
    }

    fn transaction_directory(&self) -> PathBuf {
        let rotation = match self.rotation {
            RotationPolicy::Daily => "daily",
            RotationPolicy::Hourly => "hourly",
            RotationPolicy::Never => "never",
        };
        self.directory
            .join(TRANSACTION_DIRECTORY)
            .join(format!("{}-{rotation}", encode(&self.prefix)))
    }

    fn compress_plain(&self, source: &Path, cancel: &AtomicBool) -> io::Result<()> {
        let destination = source.with_file_name(format!("{}.gz", file_name(source)?));
        if destination.exists() {
            if gzip_matches_plain(&destination, source, cancel)? {
                fs::remove_file(source)?;
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "refusing to replace archive with different source content: {}",
                    destination.display()
                ),
            ));
        }

        let temporary = temporary_path(&destination, "compress", &self.prefix);
        let result = (|| {
            let input = File::open(source)?;
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            let mut encoder = GzEncoder::new(output, Compression::default());
            copy_with_cancel(BufReader::new(input), &mut encoder, cancel)?;
            if cancel.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            let output = encoder.finish()?;
            output.sync_all()?;
            drop(output);
            publish_new(&temporary, &destination)
        })();
        if result.is_err() || cancel.load(AtomicOrdering::Acquire) {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        if cancel.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        fs::remove_file(source)
    }

    fn compact(
        &self,
        day: NaiveDate,
        fragments: &[Fragment],
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let destination = self.directory.join(format!(
            "{}.{}.merged.gz",
            self.prefix,
            day.format("%Y-%m-%d")
        ));
        let marker = PathBuf::from(format!("{}.needs-repack", destination.display()));
        let transaction = self.create_transaction(&destination, fragments)?;
        let result = (|| {
            if destination.exists() {
                validate_gzip_with_cancel(&destination, cancel)?;
            }
            if destination.exists() && suffix_is_fragments(&destination, fragments)? {
                if cancel.load(AtomicOrdering::Acquire) {
                    return Ok(());
                }
                ensure_marker(&marker)?;
                remove_fragments_transactionally(&transaction, fragments, cancel)?;
                return Ok(());
            }

            let temporary = transaction.temporary.clone();
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            let mut output = io::BufWriter::new(output);
            if destination.exists() {
                validate_gzip_with_cancel(&destination, cancel)?;
                copy_file(&destination, &mut output, cancel)?;
                if cancel.load(AtomicOrdering::Acquire) {
                    return Ok(());
                }
            }
            for fragment in fragments {
                if cancel.load(AtomicOrdering::Acquire) {
                    return Ok(());
                }
                validate_gzip_with_cancel(&fragment.path, cancel)?;
                copy_file(&fragment.path, &mut output, cancel)?;
                if cancel.load(AtomicOrdering::Acquire) {
                    return Ok(());
                }
            }
            if cancel.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            output.flush()?;
            let output = output.into_inner().map_err(|error| error.into_error())?;
            output.sync_all()?;
            drop(output);
            publish_replace(&temporary, &destination)?;
            transaction.mark_published()?;
            if cancel.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            ensure_marker(&marker)?;
            remove_fragments_transactionally(&transaction, fragments, cancel)?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                let _ = transaction.remove();
                Ok(())
            }
            Err(error) => {
                let _ = transaction.remove_temporary();
                Err(error)
            }
        }
    }

    fn repack(&self, now: SystemTime, cancel: &AtomicBool) -> io::Result<()> {
        let mut maintenance_error = None;
        for path in scan_files(&self.directory, &mut maintenance_error, cancel)? {
            if cancel.load(AtomicOrdering::Acquire) {
                return maintenance_error.map_or(Ok(()), Err);
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(marker_name) = name.strip_prefix(&format!("{}.", self.prefix)) else {
                continue;
            };
            let Some(archive_name) = marker_name.strip_suffix(".needs-repack") else {
                continue;
            };
            if !archive_name.ends_with(".merged.gz") {
                continue;
            }
            let old_enough = match is_old_enough(&path, now, REPACK_MIN_AGE) {
                Ok(old_enough) => old_enough,
                Err(error) => {
                    record_maintenance_error(
                        &mut maintenance_error,
                        format!("checking repack marker {}", path.display()),
                        error,
                    );
                    continue;
                }
            };
            if !old_enough {
                continue;
            }
            let archive = self
                .directory
                .join(format!("{}.{}", self.prefix, archive_name));
            match is_valid_gzip_with_cancel(&archive, cancel) {
                Ok(true) => {}
                Ok(false) => {
                    record_maintenance_error(
                        &mut maintenance_error,
                        format!("validating repack archive {}", archive.display()),
                        io::Error::new(io::ErrorKind::InvalidData, "invalid gzip archive"),
                    );
                    continue;
                }
                Err(error) => {
                    if error.kind() == io::ErrorKind::Interrupted
                        && cancel.load(AtomicOrdering::Acquire)
                    {
                        return Err(error);
                    }
                    record_maintenance_error(
                        &mut maintenance_error,
                        format!("validating repack archive {}", archive.display()),
                        error,
                    );
                    continue;
                }
            }
            let temporary = temporary_path(&archive, "repack", &self.prefix);
            let result = (|| {
                let source = File::open(&archive)?;
                let output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                let mut encoder = GzEncoder::new(output, Compression::default());
                let mut decoder = MultiGzDecoder::new(BufReader::new(source));
                copy_with_cancel(&mut decoder, &mut encoder, cancel)?;
                if cancel.load(AtomicOrdering::Acquire) {
                    return Ok(());
                }
                // Close the source before replacing the destination. Windows
                // rejects a rename while any reader still owns the archive.
                drop(decoder);
                let output = encoder.finish()?;
                output.sync_all()?;
                drop(output);
                publish_replace(&temporary, &archive)?;
                fs::remove_file(&path)
            })();
            if result.is_err() || cancel.load(AtomicOrdering::Acquire) {
                let _ = fs::remove_file(&temporary);
            }
            if let Err(error) = result {
                if error.kind() == io::ErrorKind::Interrupted
                    && cancel.load(AtomicOrdering::Acquire)
                {
                    return Err(error);
                }
                record_maintenance_error(
                    &mut maintenance_error,
                    format!("repacking {}", archive.display()),
                    error,
                );
            }
        }
        maintenance_error.map_or(Ok(()), Err)
    }

    fn create_transaction(
        &self,
        destination: &Path,
        fragments: &[Fragment],
    ) -> io::Result<Transaction> {
        let directory = self.transaction_directory();
        fs::create_dir_all(&directory)?;
        let id = unique_id();
        let manifest = directory.join(format!("{id}{TRANSACTION_SUFFIX}"));
        let temporary = directory.join(format!("{id}.tmp"));
        let staging = directory.join(format!("{id}.sources"));
        let mut contents = String::from("phase=prepared\n");
        contents.push_str(&format!("target={}\n", encode(file_name(destination)?)));
        contents.push_str(&format!("temporary={}\n", encode(file_name(&temporary)?)));
        contents.push_str(&format!("staging={}\n", encode(file_name(&staging)?)));
        contents.push_str(&format!("source-count={}\n", fragments.len()));
        for fragment in fragments {
            contents.push_str(&format!("source={}\n", encode(file_name(&fragment.path)?)));
        }
        contents.push_str("end\n");
        let mut manifest_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)?;
        manifest_file.write_all(contents.as_bytes())?;
        manifest_file.sync_all()?;
        sync_directory(&directory)?;
        Ok(Transaction {
            manifest,
            temporary,
            staging,
        })
    }

    fn recover_transactions(&self, cancel: &AtomicBool) -> io::Result<()> {
        let directory = self.transaction_directory();
        if !directory.exists() {
            return Ok(());
        }
        let mut maintenance_error = None;
        for path in scan_files(&directory, &mut maintenance_error, cancel)? {
            if cancel.load(AtomicOrdering::Acquire) {
                return maintenance_error.map_or(Ok(()), Err);
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("manifest") {
                continue;
            }
            record_maintenance_result(
                &mut maintenance_error,
                format!("recovering transaction {}", path.display()),
                self.recover_transaction(&path, cancel),
                cancel,
            )?;
        }
        maintenance_error.map_or(Ok(()), Err)
    }

    fn recover_transaction(&self, path: &Path, cancel: &AtomicBool) -> io::Result<()> {
        let contents = fs::read_to_string(path)?;
        let (transaction, target, fragments, published) =
            self.parse_transaction(path, &contents).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete or invalid archive manifest",
                )
            })?;
        // A partial manifest may be the only recovery record after a process
        // stop. Only a successfully parsed transaction may be cleaned up.
        let _ = fs::remove_file(transaction.state_temporary());
        restore_staged_fragments(&transaction, &fragments, cancel)?;
        let all_sources_exist = fragments.iter().all(|fragment| fragment.path.is_file());
        let archive_published = if target.is_file() && !fragments.is_empty() {
            validate_gzip_with_cancel(&target, cancel).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("validating {}: {error}", target.display()),
                )
            })?;
            all_sources_exist && suffix_is_fragments(&target, &fragments)?
        } else {
            false
        };
        if archive_published {
            remove_fragments_transactionally(&transaction, &fragments, cancel)?;
            let marker = PathBuf::from(format!("{}.needs-repack", target.display()));
            ensure_marker(&marker)?;
            let _ = transaction.remove();
        } else if published {
            // Without a complete suffix proof, keep the manifest and staged
            // copies; they may be the only recoverable source fragments.
        } else if transaction.staging.is_dir() {
            if all_sources_exist && !target.exists() {
                let _ = transaction.remove();
            }
        } else {
            let _ = transaction.remove();
        }
        Ok(())
    }

    fn parse_transaction(
        &self,
        manifest: &Path,
        contents: &str,
    ) -> Option<(Transaction, PathBuf, Vec<Fragment>, bool)> {
        let id = manifest.file_stem()?.to_str()?;
        if !is_transaction_id(id) || !contents.ends_with("\nend\n") {
            return None;
        }
        let mut lines = contents.lines();
        let published = match lines.next()? {
            "phase=prepared" => false,
            "phase=published" => true,
            _ => return None,
        };
        let target = decode_filename(lines.next()?.strip_prefix("target=")?)?;
        let day = self.parse_merged_name(&target)?;
        let temporary = decode_filename(lines.next()?.strip_prefix("temporary=")?)?;
        let staging = decode_filename(lines.next()?.strip_prefix("staging=")?)?;
        if temporary != format!("{id}.tmp") || staging != format!("{id}.sources") {
            return None;
        }
        let count: usize = lines.next()?.strip_prefix("source-count=")?.parse().ok()?;
        if count < COMPACTION_MIN_FRAGMENTS {
            return None;
        }
        let mut fragments = Vec::new();
        for _ in 0..count {
            let name = decode_filename(lines.next()?.strip_prefix("source=")?)?;
            if self.parse_fragment_name(&name)?.period.date() != day {
                return None;
            }
            let path = self.directory.join(name);
            if fragments
                .iter()
                .any(|fragment: &Fragment| fragment.path == path)
            {
                return None;
            }
            fragments.push(Fragment { path });
        }
        if lines.next()? != "end" || lines.next().is_some() {
            return None;
        }
        let directory = self.transaction_directory();
        Some((
            Transaction {
                manifest: manifest.to_path_buf(),
                temporary: directory.join(temporary),
                staging: directory.join(staging),
            },
            self.directory.join(target),
            fragments,
            published,
        ))
    }

    fn remove_stale_temporary_files(&self, cancel: &AtomicBool) -> io::Result<()> {
        let mut maintenance_error = None;
        for path in scan_files(&self.directory, &mut maintenance_error, cancel)? {
            if cancel.load(AtomicOrdering::Acquire) {
                return maintenance_error.map_or(Ok(()), Err);
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let namespace = format!(".service-daemon-archive-{}-", encode(&self.prefix));
            if name.strip_prefix(&namespace).is_some_and(|tail| {
                ["compress-", "repack-"]
                    .iter()
                    .any(|kind| tail.strip_prefix(kind).is_some_and(is_transaction_id))
            }) {
                record_maintenance_result(
                    &mut maintenance_error,
                    format!("removing temporary file {}", path.display()),
                    fs::remove_file(&path),
                    cancel,
                )?;
            }
        }
        maintenance_error.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Period {
    Daily(NaiveDate),
    Hourly(NaiveDateTime),
}

impl Period {
    fn from_system_time(rotation: RotationPolicy, now: SystemTime) -> Self {
        let now: DateTime<Utc> = now.into();
        match rotation {
            RotationPolicy::Daily | RotationPolicy::Never => Self::Daily(now.date_naive()),
            RotationPolicy::Hourly => Self::Hourly(
                now.naive_utc()
                    .with_minute(0)
                    .and_then(|value| value.with_second(0))
                    .and_then(|value| value.with_nanosecond(0))
                    .unwrap_or_else(|| now.naive_utc()),
            ),
        }
    }

    fn date(self) -> NaiveDate {
        match self {
            Self::Daily(date) => date,
            Self::Hourly(date_time) => date_time.date(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FragmentName {
    period: Period,
}

#[derive(Debug)]
struct Fragment {
    path: PathBuf,
}

struct Transaction {
    manifest: PathBuf,
    temporary: PathBuf,
    staging: PathBuf,
}

impl Transaction {
    fn state_temporary(&self) -> PathBuf {
        self.manifest.with_extension("state.tmp")
    }

    fn mark_published(&self) -> io::Result<()> {
        let contents = fs::read_to_string(&self.manifest)?;
        let replacement = self.state_temporary();
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&replacement)?;
        output.write_all(
            contents
                .replace("phase=prepared", "phase=published")
                .as_bytes(),
        )?;
        output.sync_all()?;
        drop(output);
        let result = fs::rename(&replacement, &self.manifest);
        if result.is_err() {
            let _ = fs::remove_file(replacement);
        } else {
            sync_directory(self.manifest.parent().unwrap_or_else(|| Path::new(".")))?;
        }
        result
    }

    fn remove(&self) -> io::Result<()> {
        let _ = fs::remove_file(&self.manifest);
        let _ = fs::remove_file(self.state_temporary());
        let _ = fs::remove_file(&self.temporary);
        let _ = fs::remove_dir_all(&self.staging);
        Ok(())
    }

    fn remove_temporary(&self) -> io::Result<()> {
        let _ = fs::remove_file(&self.temporary);
        Ok(())
    }
}

fn parse_period(value: &str, rotation: RotationPolicy) -> Option<Period> {
    match rotation {
        RotationPolicy::Daily => NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .map(Period::Daily),
        RotationPolicy::Hourly => {
            let value = format!("{value}:00");
            NaiveDateTime::parse_from_str(&value, "%Y-%m-%d-%H:%M")
                .ok()
                .map(Period::Hourly)
        }
        RotationPolicy::Never => None,
    }
}

fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("archive path is not a UTF-8 file name: {}", path.display()),
            )
        })
}

fn is_active(path: &Path, active: Option<&Path>) -> bool {
    let Some(active) = active else {
        return false;
    };
    path == active || path.file_name() == active.file_name()
}

fn is_old_enough(path: &Path, now: SystemTime, age: Duration) -> io::Result<bool> {
    let modified = fs::metadata(path)?.modified()?;
    Ok(now
        .duration_since(modified)
        .is_ok_and(|elapsed| elapsed > age))
}

fn record_maintenance_error(
    first_error: &mut Option<io::Error>,
    context: impl AsRef<str>,
    error: io::Error,
) {
    let context = context.as_ref();
    // Archive failures must stay visible even when the file logger itself is
    // failing or shutting down. Do not feed them back into its logging queue.
    eprintln!("ERROR service-daemon log archive candidate failed: {context}: {error}");
    if first_error.is_none() {
        *first_error = Some(io::Error::new(error.kind(), format!("{context}: {error}")));
    }
}

fn record_maintenance_result(
    first_error: &mut Option<io::Error>,
    context: impl AsRef<str>,
    result: io::Result<()>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::Interrupted && cancel.load(AtomicOrdering::Acquire) {
            return Err(error);
        }
        record_maintenance_error(first_error, context, error);
    }
    Ok(())
}

fn scan_files(
    directory: &Path,
    first_error: &mut Option<io::Error>,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        if cancel.load(AtomicOrdering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive scan cancelled",
            ));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                record_maintenance_error(
                    first_error,
                    format!("reading directory entry in {}", directory.display()),
                    error,
                );
                continue;
            }
        };
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => files.push(path),
            Ok(_) => {}
            Err(error) => record_maintenance_error(
                first_error,
                format!("reading metadata for {}", path.display()),
                error,
            ),
        }
    }
    files.sort();
    Ok(files)
}

fn temporary_path(destination: &Path, kind: &str, prefix: &str) -> PathBuf {
    destination.with_file_name(format!(
        ".service-daemon-archive-{}-{kind}-{}-{}",
        encode(prefix),
        std::process::id(),
        unique_id()
    ))
}

fn unique_id() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn copy_with_cancel<R: Read, W: Write>(
    mut input: R,
    mut output: W,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
    loop {
        if cancel.load(AtomicOrdering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive operation cancelled",
            ));
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
    }
    Ok(())
}

fn copy_file(path: &Path, output: &mut impl Write, cancel: &AtomicBool) -> io::Result<()> {
    copy_with_cancel(BufReader::new(File::open(path)?), output, cancel)
}

fn publish_new(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)?;
    sync_directory(destination.parent().unwrap_or_else(|| Path::new(".")))
}

fn publish_replace(temporary: &Path, destination: &Path) -> io::Result<()> {
    // std::fs::rename replaces existing files on Unix and Windows. Keeping
    // this as one operation avoids a crash window with no published archive.
    fs::rename(temporary, destination)?;
    sync_directory(destination.parent().unwrap_or_else(|| Path::new(".")))
}

fn validate_gzip_with_cancel(path: &Path, cancel: &AtomicBool) -> io::Result<()> {
    let mut decoder = MultiGzDecoder::new(BufReader::new(File::open(path)?));
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
    loop {
        if cancel.load(AtomicOrdering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive operation cancelled",
            ));
        }
        if decoder.read(&mut buffer)? == 0 {
            break;
        }
    }
    Ok(())
}

fn is_valid_gzip_with_cancel(path: &Path, cancel: &AtomicBool) -> io::Result<bool> {
    match validate_gzip_with_cancel(path, cancel) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => Ok(false),
        Err(error) => Err(error),
    }
}

fn gzip_matches_plain(archive: &Path, source: &Path, cancel: &AtomicBool) -> io::Result<bool> {
    let mut decoder = MultiGzDecoder::new(BufReader::new(File::open(archive)?));
    let mut source = BufReader::new(File::open(source)?);
    let mut archive_buffer = [0_u8; COPY_BUFFER_SIZE];
    let mut source_buffer = [0_u8; COPY_BUFFER_SIZE];
    let mut archive_len = 0_usize;
    let mut source_len = 0_usize;
    let mut archive_offset = 0_usize;
    let mut source_offset = 0_usize;
    loop {
        if cancel.load(AtomicOrdering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive operation cancelled",
            ));
        }
        if source_offset == source_len {
            source_len = source.read(&mut source_buffer)?;
            source_offset = 0;
        }
        if archive_offset == archive_len {
            archive_len = decoder.read(&mut archive_buffer)?;
            archive_offset = 0;
        }
        if source_len == 0 || archive_len == 0 {
            return Ok(source_len == archive_len);
        }
        let length = (source_len - source_offset).min(archive_len - archive_offset);
        if source_buffer[source_offset..source_offset + length]
            != archive_buffer[archive_offset..archive_offset + length]
        {
            return Ok(false);
        }
        source_offset += length;
        archive_offset += length;
    }
}

fn remove_fragments_transactionally(
    transaction: &Transaction,
    fragments: &[Fragment],
    cancel: &AtomicBool,
) -> io::Result<()> {
    if fragments.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(&transaction.staging)?;
    for (index, fragment) in fragments.iter().enumerate() {
        if cancel.load(AtomicOrdering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive operation cancelled",
            ));
        }
        let staged = staged_fragment_path(&transaction.staging, index);
        if staged.exists() {
            // A staging path is published only after its private copy has
            // been flushed and renamed, so its presence is the completion
            // marker. Reusing it also avoids replacing a durable backup on
            // platforms where rename cannot overwrite an open destination.
            continue;
        }
        if let Err(error) = copy_file_durable(&fragment.path, &staged, cancel) {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
    }
    sync_directory(&transaction.staging)?;
    for fragment in fragments {
        if cancel.load(AtomicOrdering::Acquire) {
            let _ = restore_staged_fragments(transaction, fragments, cancel);
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive operation cancelled",
            ));
        }
        if let Err(error) = fs::remove_file(&fragment.path) {
            let _ = restore_staged_fragments(transaction, fragments, cancel);
            return Err(error);
        }
    }
    // The archive has already been published.  Failure to remove a backup is
    // harmless and the next maintenance pass can clean the transaction dir.
    let _ = fs::remove_dir_all(&transaction.staging);
    Ok(())
}

fn restore_staged_fragments(
    transaction: &Transaction,
    fragments: &[Fragment],
    cancel: &AtomicBool,
) -> io::Result<()> {
    if !transaction.staging.is_dir() {
        return Ok(());
    }
    for (index, fragment) in fragments.iter().enumerate() {
        let staged = staged_fragment_path(&transaction.staging, index);
        if !staged.is_file() {
            continue;
        }
        if cancel.load(AtomicOrdering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "log archive operation cancelled",
            ));
        }
        // A source that still exists is authoritative.  It may be newer than
        // the staged backup, and replacing it with a partial or stale backup
        // would destroy recoverable log content.  Staging files are published
        // atomically, so only a missing source is eligible for restoration.
        match fs::symlink_metadata(&fragment.path) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Reject incomplete backups from older interrupted writes as well.
        validate_gzip_with_cancel(&staged, cancel)?;
        // Linking an already durable backup publishes it atomically without
        // replacing a source created since the existence check. If the file
        // system cannot link it, retain staging and report the recovery error.
        match fs::hard_link(&staged, &fragment.path) {
            Ok(()) => {
                sync_directory(fragment.path.parent().unwrap_or_else(|| Path::new(".")))?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn staged_fragment_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(format!("{index}.source"))
}

fn copy_file_durable(source: &Path, destination: &Path, cancel: &AtomicBool) -> io::Result<()> {
    let destination_name = file_name(destination)?;
    let temporary = destination.with_file_name(format!(
        ".service-daemon-archive-copy-{destination_name}-{}-{}.tmp",
        std::process::id(),
        unique_id()
    ));
    let input = File::open(source)?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        let mut output = io::BufWriter::new(output);
        copy_with_cancel(BufReader::new(input), &mut output, cancel)?;
        output.flush()?;
        let output = output.into_inner().map_err(|error| error.into_error())?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, destination)?;
        sync_directory(destination.parent().unwrap_or_else(|| Path::new(".")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn ensure_marker(marker: &Path) -> io::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(mut file) => file.write_all(b"needs repack\n"),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn suffix_is_fragments(destination: &Path, fragments: &[Fragment]) -> io::Result<bool> {
    let total: u64 = fragments
        .iter()
        .map(|fragment| fs::metadata(&fragment.path).map(|metadata| metadata.len()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .sum();
    let mut destination_file = File::open(destination)?;
    let destination_len = destination_file.metadata()?.len();
    if destination_len < total {
        return Ok(false);
    }
    destination_file.seek(SeekFrom::Start(destination_len - total))?;
    let mut left = [0_u8; COPY_BUFFER_SIZE];
    for fragment in fragments {
        let mut source = BufReader::new(File::open(&fragment.path)?);
        loop {
            let read = source.read(&mut left)?;
            if read == 0 {
                break;
            }
            let mut expected = vec![0_u8; read];
            destination_file.read_exact(&mut expected)?;
            if expected != left[..read] {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode(value: &str) -> Result<String, ()> {
    if !value.is_ascii() || !value.len().is_multiple_of(2) {
        return Err(());
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    String::from_utf8(bytes).map_err(|_| ())
}

fn is_transaction_id(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(character) if character.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
        && matches!(value.chars().next_back(), Some(character) if character.is_ascii_digit())
}

fn decode_filename(value: &str) -> Option<String> {
    let name = decode(value).ok()?;
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn test_directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("service-daemon-archive-{name}-{}", unique_id()));
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    fn write_gzip(path: &Path, content: &[u8]) {
        let output = File::create(path).expect("create gzip");
        let mut encoder = GzEncoder::new(output, Compression::default());
        encoder.write_all(content).expect("write gzip");
        encoder.finish().expect("finish gzip");
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn compresses_closed_plain_file_and_round_trips() {
        let directory = test_directory("compress");
        let source = directory.join("latest.log.2026-01-01");
        fs::write(&source, b"hello\n").expect("write source");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        store
            .maintain(
                None,
                SystemTime::now() + Duration::from_secs(86_400),
                &AtomicBool::new(false),
            )
            .expect("maintain");
        let archive = directory.join("latest.log.2026-01-01.gz");
        assert!(!source.exists());
        assert!(!fs::read_to_string(&archive).is_ok());
        validate_gzip_with_cancel(&archive, &AtomicBool::new(false)).expect("valid gzip");
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(&archive).expect("archive"))
            .read_to_string(&mut decoded)
            .expect("decode archive");
        assert_eq!(decoded, "hello\n");
        cleanup(&directory);
    }

    #[test]
    fn custom_prefix_is_preserved_when_archiving() {
        let directory = test_directory("custom-prefix");
        let source = directory.join("application.log.2026-01-01");
        fs::write(&source, b"custom\n").expect("write source");
        let store = ArchiveStore::new(
            directory.clone(),
            "application.log".into(),
            RotationPolicy::Daily,
        );
        store
            .maintain(
                None,
                SystemTime::now() + Duration::from_secs(86_400),
                &AtomicBool::new(false),
            )
            .expect("maintain");
        assert!(directory.join("application.log.2026-01-01.gz").is_file());
        assert!(!directory.join("latest.log.2026-01-01.gz").exists());
        cleanup(&directory);
    }

    #[test]
    fn active_file_is_never_archived() {
        let directory = test_directory("active");
        let source = directory.join("latest.log.2026-01-01");
        fs::write(&source, b"active\n").expect("write source");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        store
            .maintain(
                Some(&source),
                SystemTime::now() + Duration::from_secs(86_400),
                &AtomicBool::new(false),
            )
            .expect("maintain");
        assert!(source.exists());
        assert!(!directory.join("latest.log.2026-01-01.gz").exists());
        cleanup(&directory);
    }

    #[test]
    fn cancellation_does_not_publish_partial_archive() {
        let directory = test_directory("cancel");
        let source = directory.join("latest.log.2026-01-01");
        fs::write(&source, vec![b'x'; COPY_BUFFER_SIZE * 2]).expect("write source");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        let cancel = AtomicBool::new(true);
        let result = store.compress_plain(&source, &cancel);
        assert!(result.is_err());
        assert!(source.exists());
        assert!(!directory.join("latest.log.2026-01-01.gz").exists());
        cleanup(&directory);
    }

    #[test]
    fn mismatched_existing_gzip_preserves_plain_source() {
        let directory = test_directory("mismatched-existing");
        let source = directory.join("latest.log.2026-01-01");
        let destination = directory.join("latest.log.2026-01-01.gz");
        fs::write(&source, b"new contents\n").expect("write source");
        write_gzip(&destination, b"old contents\n");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        let result = store.compress_plain(&source, &AtomicBool::new(false));
        assert!(result.is_err());
        assert!(source.is_file());
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(destination).expect("archive"))
            .read_to_string(&mut decoded)
            .expect("decode archive");
        assert_eq!(decoded, "old contents\n");
        cleanup(&directory);
    }

    #[test]
    fn compacts_three_fragments_and_repack_round_trips() {
        let directory = test_directory("compact");
        for index in 0..3 {
            write_gzip(
                &directory.join(format!("latest.log.2026-01-01-0{index}.gz")),
                format!("{index}\n").as_bytes(),
            );
        }
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let now = SystemTime::now() + Duration::from_secs(2 * 86_400);
        store
            .maintain(None, now, &AtomicBool::new(false))
            .expect("compact");
        let merged = directory.join("latest.log.2026-01-01.merged.gz");
        assert!(merged.exists());
        assert!(
            directory
                .join("latest.log.2026-01-01.merged.gz.needs-repack")
                .exists()
        );
        for index in 0..3 {
            assert!(
                !directory
                    .join(format!("latest.log.2026-01-01-0{index}.gz"))
                    .exists()
            );
        }
        store
            .maintain(
                None,
                now + Duration::from_secs(31 * 86_400),
                &AtomicBool::new(false),
            )
            .expect("repack");
        assert!(
            !directory
                .join("latest.log.2026-01-01.merged.gz.needs-repack")
                .exists()
        );
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(merged).expect("merged"))
            .read_to_string(&mut decoded)
            .expect("decode merged");
        assert_eq!(decoded, "0\n1\n2\n");
        cleanup(&directory);
    }

    #[test]
    fn compaction_requires_more_than_24_hours() {
        let directory = test_directory("compact-boundary");
        let now = SystemTime::now();
        let age = Duration::from_secs(24 * 60 * 60);
        // Keep the boundary just inside the threshold with enough margin for
        // filesystems whose modification timestamps have coarse precision.
        let just_under = age
            .checked_sub(Duration::from_secs(2))
            .expect("boundary margin");
        for index in 0..3 {
            let path = directory.join(format!("latest.log.2026-01-01-0{index}.gz"));
            write_gzip(&path, format!("{index}\n").as_bytes());
            File::open(&path)
                .expect("fragment")
                .set_modified(now.checked_sub(just_under).expect("age"))
                .expect("set fragment time");
        }
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        store
            .maintain(None, now, &AtomicBool::new(false))
            .expect("maintain at boundary");
        assert!(!directory.join("latest.log.2026-01-01.merged.gz").exists());
        cleanup(&directory);

        let directory = test_directory("compact-after-boundary");
        let just_over = age + Duration::from_secs(2);
        for index in 0..3 {
            let path = directory.join(format!("latest.log.2026-01-01-0{index}.gz"));
            write_gzip(&path, format!("{index}\n").as_bytes());
            File::open(&path)
                .expect("fragment")
                .set_modified(now.checked_sub(just_over).expect("age"))
                .expect("set fragment time");
        }
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        store
            .maintain(None, now, &AtomicBool::new(false))
            .expect("compact after boundary");
        assert!(directory.join("latest.log.2026-01-01.merged.gz").is_file());
        cleanup(&directory);
    }

    #[test]
    fn repack_requires_more_than_30_days() {
        let directory = test_directory("repack-boundary");
        let archive = directory.join("latest.log.2026-01-01.merged.gz");
        write_gzip(&archive, b"merged\n");
        let marker = directory.join("latest.log.2026-01-01.merged.gz.needs-repack");
        fs::write(&marker, b"needs repack\n").expect("marker");
        let now = SystemTime::now();
        let age = Duration::from_secs(30 * 24 * 60 * 60);
        let just_under = age
            .checked_sub(Duration::from_secs(2))
            .expect("boundary margin");
        File::open(&marker)
            .expect("marker")
            .set_modified(now.checked_sub(just_under).expect("age"))
            .expect("set marker time");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        store
            .maintain(None, now, &AtomicBool::new(false))
            .expect("maintain at boundary");
        assert!(archive.is_file());
        assert!(marker.is_file());
        cleanup(&directory);

        let directory = test_directory("repack-after-boundary");
        let archive = directory.join("latest.log.2026-01-01.merged.gz");
        write_gzip(&archive, b"merged\n");
        let marker = directory.join("latest.log.2026-01-01.merged.gz.needs-repack");
        fs::write(&marker, b"needs repack\n").expect("marker");
        File::open(&marker)
            .expect("marker")
            .set_modified(now.checked_sub(age + Duration::from_secs(2)).expect("age"))
            .expect("set marker time");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        store
            .maintain(None, now, &AtomicBool::new(false))
            .expect("repack after boundary");
        assert!(!marker.exists());
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(&archive).expect("archive"))
            .read_to_string(&mut decoded)
            .expect("decode archive");
        assert_eq!(decoded, "merged\n");
        cleanup(&directory);
    }

    #[test]
    fn corrupt_fragment_is_preserved() {
        let directory = test_directory("corrupt");
        for index in 0..2 {
            write_gzip(
                &directory.join(format!("latest.log.2026-01-01-0{index}.gz")),
                b"valid\n",
            );
        }
        fs::write(directory.join("latest.log.2026-01-01-02.gz"), b"not gzip")
            .expect("write corrupt");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let result = store.maintain(
            None,
            SystemTime::now() + Duration::from_secs(2 * 86_400),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert!(directory.join("latest.log.2026-01-01-02.gz").exists());
        cleanup(&directory);
    }

    #[test]
    fn corrupt_plain_candidate_does_not_block_later_plain_candidate() {
        let directory = test_directory("corrupt-plain-followed-by-valid");
        let corrupt_source = directory.join("latest.log.2026-01-01");
        let corrupt_archive = directory.join("latest.log.2026-01-01.gz");
        fs::write(&corrupt_source, b"corrupt source\n").expect("corrupt source");
        fs::write(&corrupt_archive, b"not gzip").expect("corrupt archive");

        let valid_source = directory.join("latest.log.2026-01-02");
        let valid_archive = directory.join("latest.log.2026-01-02.gz");
        fs::write(&valid_source, b"valid source\n").expect("valid source");

        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        let result = store.maintain(
            None,
            SystemTime::now() + Duration::from_secs(86_400),
            &AtomicBool::new(false),
        );

        let error = result.expect_err("corrupt candidate must remain observable");
        assert!(!error.to_string().is_empty());
        assert!(corrupt_source.is_file());
        assert!(corrupt_archive.is_file());
        assert!(valid_archive.is_file());
        assert!(!valid_source.exists());
        cleanup(&directory);
    }

    #[test]
    fn corrupt_compaction_group_does_not_block_later_group() {
        let directory = test_directory("corrupt-compaction-followed-by-valid");
        let now = SystemTime::now();
        let old = now
            .checked_sub(COMPACTION_MIN_AGE + Duration::from_secs(60))
            .expect("old fragment time");

        for index in 0..3 {
            let path = directory.join(format!("latest.log.2026-01-01-0{index}.gz"));
            if index == 2 {
                fs::write(&path, b"not gzip").expect("corrupt fragment");
            } else {
                write_gzip(&path, format!("bad-{index}\n").as_bytes());
            }
            File::open(&path)
                .expect("fragment")
                .set_modified(old)
                .expect("set fragment time");
        }
        for index in 0..3 {
            let path = directory.join(format!("latest.log.2026-01-02-0{index}.gz"));
            write_gzip(&path, format!("good-{index}\n").as_bytes());
            File::open(&path)
                .expect("fragment")
                .set_modified(old)
                .expect("set fragment time");
        }

        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let result = store.maintain(None, now, &AtomicBool::new(false));

        let error = result.expect_err("corrupt group must remain observable");
        assert!(!error.to_string().is_empty());
        assert!(directory.join("latest.log.2026-01-01-02.gz").is_file());
        assert!(!directory.join("latest.log.2026-01-01.merged.gz").exists());
        let merged = directory.join("latest.log.2026-01-02.merged.gz");
        assert!(merged.is_file());
        for index in 0..3 {
            assert!(
                directory
                    .join(format!("latest.log.2026-01-01-0{index}.gz"))
                    .is_file()
            );
        }
        for index in 0..3 {
            assert!(
                !directory
                    .join(format!("latest.log.2026-01-02-0{index}.gz"))
                    .exists()
            );
        }
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(merged).expect("merged archive"))
            .read_to_string(&mut decoded)
            .expect("decode merged archive");
        assert_eq!(decoded, "good-0\ngood-1\ngood-2\n");
        cleanup(&directory);
    }

    #[test]
    fn corrupt_repack_candidate_does_not_block_later_repack() {
        let directory = test_directory("corrupt-repack-followed-by-valid");
        let now = SystemTime::now();
        let old = now
            .checked_sub(REPACK_MIN_AGE + Duration::from_secs(60))
            .expect("old marker time");

        let corrupt_archive = directory.join("latest.log.2026-01-01.merged.gz");
        let corrupt_marker = directory.join("latest.log.2026-01-01.merged.gz.needs-repack");
        fs::write(&corrupt_archive, b"not gzip").expect("corrupt archive");
        fs::write(&corrupt_marker, b"needs repack\n").expect("corrupt marker");
        File::open(&corrupt_marker)
            .expect("corrupt marker")
            .set_modified(old)
            .expect("set marker time");

        let valid_archive = directory.join("latest.log.2026-01-02.merged.gz");
        let valid_marker = directory.join("latest.log.2026-01-02.merged.gz.needs-repack");
        write_gzip(&valid_archive, b"valid merged\n");
        fs::write(&valid_marker, b"needs repack\n").expect("valid marker");
        File::open(&valid_marker)
            .expect("valid marker")
            .set_modified(old)
            .expect("set marker time");

        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        let result = store.maintain(None, now, &AtomicBool::new(false));

        let error = result.expect_err("corrupt repack must remain observable");
        assert!(!error.to_string().is_empty());
        assert!(corrupt_archive.is_file());
        assert!(corrupt_marker.is_file());
        assert!(valid_archive.is_file());
        assert!(!valid_marker.exists());
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(valid_archive).expect("valid archive"))
            .read_to_string(&mut decoded)
            .expect("decode valid archive");
        assert_eq!(decoded, "valid merged\n");
        cleanup(&directory);
    }

    #[test]
    fn failed_destination_validation_preserves_all_sources() {
        let directory = test_directory("destination-failure");
        for index in 0..3 {
            write_gzip(
                &directory.join(format!("latest.log.2026-01-01-0{index}.gz")),
                format!("{index}\n").as_bytes(),
            );
        }
        let destination = directory.join("latest.log.2026-01-01.merged.gz");
        fs::write(&destination, b"not gzip").expect("invalid destination");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let result = store.maintain(
            None,
            SystemTime::now() + Duration::from_secs(2 * 86_400),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert!(destination.is_file());
        for index in 0..3 {
            assert!(
                directory
                    .join(format!("latest.log.2026-01-01-0{index}.gz"))
                    .is_file()
            );
        }
        cleanup(&directory);
    }

    #[test]
    fn corrupt_merged_tail_does_not_delete_fragments() {
        let directory = test_directory("corrupt-tail");
        let mut suffix = Vec::new();
        for index in 0..3 {
            let path = directory.join(format!("latest.log.2026-01-01-0{index}.gz"));
            write_gzip(&path, format!("{index}\n").as_bytes());
            suffix.extend(fs::read(path).expect("fragment bytes"));
        }
        let destination = directory.join("latest.log.2026-01-01.merged.gz");
        let mut corrupt = b"not gzip\n".to_vec();
        corrupt.extend(suffix);
        fs::write(&destination, corrupt).expect("destination");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let result = store.maintain(
            None,
            SystemTime::now() + Duration::from_secs(2 * 86_400),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        for index in 0..3 {
            assert!(
                directory
                    .join(format!("latest.log.2026-01-01-0{index}.gz"))
                    .is_file()
            );
        }
        cleanup(&directory);
    }

    #[test]
    fn recovery_restores_staged_sources_when_target_was_not_published() {
        let directory = test_directory("recovery");
        let paths = (0..3)
            .map(|index| directory.join(format!("latest.log.2026-01-01-0{index}.gz")))
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate() {
            write_gzip(path, format!("{index}\n").as_bytes());
        }
        let fragments = paths
            .iter()
            .cloned()
            .map(|path| Fragment { path })
            .collect::<Vec<_>>();
        let destination = directory.join("latest.log.2026-01-01.merged.gz");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let transaction = store
            .create_transaction(&destination, &fragments)
            .expect("transaction");
        fs::create_dir_all(&transaction.staging).expect("staging");
        for (index, path) in paths.iter().enumerate() {
            fs::copy(path, staged_fragment_path(&transaction.staging, index))
                .expect("stage source");
            fs::remove_file(path).expect("remove source");
        }

        store
            .maintain(None, SystemTime::now(), &AtomicBool::new(false))
            .expect("recover transaction");
        for path in &paths {
            assert!(path.is_file(), "recovery must restore {}", path.display());
        }
        assert!(!transaction.manifest.exists());
        assert!(!transaction.staging.exists());
        cleanup(&directory);
    }

    #[test]
    fn source_delete_failure_restores_every_staged_fragment() {
        let directory = test_directory("delete-rollback");
        let paths = (0..2)
            .map(|index| directory.join(format!("latest.log.2026-01-01-0{index}.gz")))
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate() {
            write_gzip(path, format!("{index}\n").as_bytes());
        }
        let fragments = paths
            .iter()
            .cloned()
            .map(|path| Fragment { path })
            .collect::<Vec<_>>();
        let staging = directory.join("staging");
        fs::create_dir_all(&staging).expect("staging");
        for (index, path) in paths.iter().enumerate() {
            fs::copy(path, staged_fragment_path(&staging, index)).expect("stage source");
        }
        // The second source disappears after the backup phase.  The first
        // deletion must be rolled back when the second deletion fails.
        fs::remove_file(&paths[1]).expect("remove source");
        let transaction = Transaction {
            manifest: directory.join("manifest"),
            temporary: directory.join("temporary"),
            staging,
        };
        let result =
            remove_fragments_transactionally(&transaction, &fragments, &AtomicBool::new(false));
        assert!(result.is_err());
        assert!(paths.iter().all(|path| path.is_file()));
        cleanup(&directory);
    }

    #[test]
    fn publish_replace_failure_keeps_temporary_file() {
        let directory = test_directory("rename-failure");
        let temporary = directory.join("archive.tmp");
        let destination = directory.join("archive.gz");
        fs::write(&temporary, b"archive\n").expect("temporary");
        fs::create_dir(&destination).expect("destination directory");

        let result = publish_replace(&temporary, &destination);

        assert!(result.is_err());
        assert!(temporary.is_file());
        assert!(destination.is_dir());
        cleanup(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_delete_sharing_violation_keeps_source_for_retry() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = test_directory("windows-lock");
        let source = directory.join("latest.log.2026-01-01");
        fs::write(&source, b"locked\n").expect("write source");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        // Allow the archiver to read the file but deny delete sharing, which
        // models another Windows process holding the rotated log open.
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&source)
            .expect("open locked source");
        let result = store.maintain(
            None,
            SystemTime::now() + Duration::from_secs(86_400),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert!(source.is_file());
        assert!(directory.join("latest.log.2026-01-01.gz").is_file());
        drop(lock);

        store
            .maintain(
                None,
                SystemTime::now() + Duration::from_secs(86_400),
                &AtomicBool::new(false),
            )
            .expect("retry after releasing lock");
        assert!(!source.exists());
        cleanup(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_repack_sharing_violation_keeps_archive_and_marker_for_retry() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = test_directory("windows-repack-lock");
        let archive = directory.join("latest.log.2026-01-01.merged.gz");
        write_gzip(&archive, b"merged\n");
        let marker = directory.join("latest.log.2026-01-01.merged.gz.needs-repack");
        fs::write(&marker, b"needs repack\n").expect("marker");
        let now = SystemTime::now();
        File::open(&marker)
            .expect("marker")
            .set_modified(
                now.checked_sub(REPACK_MIN_AGE + Duration::from_secs(2))
                    .expect("age"),
            )
            .expect("set marker time");

        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&archive)
            .expect("open locked archive");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        let result = store.maintain(
            None,
            now + Duration::from_secs(31 * 86_400),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert!(archive.is_file());
        assert!(marker.is_file());

        drop(lock);
        store
            .maintain(
                None,
                now + Duration::from_secs(31 * 86_400),
                &AtomicBool::new(false),
            )
            .expect("retry after releasing archive lock");
        assert!(archive.is_file());
        assert!(!marker.exists());
        cleanup(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_interrupted_transaction_recovers_on_next_maintenance() {
        let directory = test_directory("windows-recovery");
        let paths = (0..3)
            .map(|index| directory.join(format!("latest.log.2026-01-01-0{index}.gz")))
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate() {
            write_gzip(path, format!("{index}\n").as_bytes());
        }
        let fragments = paths
            .iter()
            .cloned()
            .map(|path| Fragment { path })
            .collect::<Vec<_>>();
        let destination = directory.join("latest.log.2026-01-01.merged.gz");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Hourly,
        );
        let transaction = store
            .create_transaction(&destination, &fragments)
            .expect("transaction");
        fs::create_dir_all(&transaction.staging).expect("staging");
        for (index, path) in paths.iter().enumerate() {
            fs::copy(path, staged_fragment_path(&transaction.staging, index))
                .expect("stage source");
            fs::remove_file(path).expect("remove source");
        }

        store
            .maintain(None, SystemTime::now(), &AtomicBool::new(false))
            .expect("recover transaction");
        assert!(paths.iter().all(|path| path.is_file()));
        assert!(!transaction.manifest.exists());
        assert!(!transaction.staging.exists());
        cleanup(&directory);
    }

    #[test]
    fn malformed_transaction_is_retained_without_blocking_maintenance() {
        let directory = test_directory("malformed-transaction");
        let source = directory.join("latest.log.2026-01-01");
        fs::write(&source, b"plain\n").expect("source");
        let store = ArchiveStore::new(
            directory.clone(),
            "latest.log".into(),
            RotationPolicy::Daily,
        );
        let transaction_directory = store.transaction_directory();
        let manifest = transaction_directory.join("123.manifest");
        let staging = transaction_directory.join("123.sources");
        fs::create_dir_all(&staging).expect("staging");
        fs::write(&manifest, b"phase=prepared\n").expect("manifest");
        fs::write(staging.join("recoverable.source"), b"recoverable\n").expect("staged source");

        store
            .maintain(
                None,
                SystemTime::now() + Duration::from_secs(86_400),
                &AtomicBool::new(false),
            )
            .expect_err("malformed recovery record must be reported");
        assert!(manifest.is_file());
        assert!(staging.is_dir());
        assert!(directory.join("latest.log.2026-01-01.gz").is_file());
        cleanup(&directory);
    }

    #[test]
    fn staged_restore_does_not_replace_existing_source() {
        let directory = test_directory("staged-existing-source");
        let source = directory.join("latest.log.2026-01-01-00.gz");
        let staged = directory.join("staged");
        fs::create_dir_all(&staged).expect("staging");
        write_gzip(&source, b"complete\n");
        let staged_source = staged.join("0.source");
        fs::write(&staged_source, b"partial staging\n").expect("partial staging");
        let transaction = Transaction {
            manifest: directory.join("manifest"),
            temporary: directory.join("temporary"),
            staging: staged,
        };
        restore_staged_fragments(
            &transaction,
            &[Fragment {
                path: source.clone(),
            }],
            &AtomicBool::new(false),
        )
        .expect("restore");
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(&source).expect("source"))
            .read_to_string(&mut decoded)
            .expect("decode source");
        assert_eq!(decoded, "complete\n");
        cleanup(&directory);
    }

    #[test]
    fn staged_restore_recovers_missing_source_from_complete_staging() {
        let directory = test_directory("staged-missing-source");
        let source = directory.join("latest.log.2026-01-01-00.gz");
        let staged = directory.join("staged");
        fs::create_dir_all(&staged).expect("staging");
        let staged_source = staged.join("0.source");
        write_gzip(&staged_source, b"recoverable\n");
        let transaction = Transaction {
            manifest: directory.join("manifest"),
            temporary: directory.join("temporary"),
            staging: staged,
        };
        restore_staged_fragments(
            &transaction,
            &[Fragment {
                path: source.clone(),
            }],
            &AtomicBool::new(false),
        )
        .expect("restore");
        let mut decoded = String::new();
        MultiGzDecoder::new(File::open(&source).expect("restored source"))
            .read_to_string(&mut decoded)
            .expect("decode source");
        assert_eq!(decoded, "recoverable\n");
        cleanup(&directory);
    }
}
