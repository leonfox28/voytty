//! Disk-backed admission shared by every daemon for this account.
use crate::user_state::{open_append, open_read, validate_directory, validate_regular_file};
use std::{
    fs::{self, File},
    io,
    path::Path,
    time::Duration,
};
use voytty_core::{
    SessionId,
    upload::{MAX_UPLOAD_EXTENSION_BYTES, TransferId},
};

use super::private_path_error;

const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const SESSION_LIMIT: Usage = Usage {
    bytes: 200_000_000,
    files: 64,
    active: 4,
};
const ACCOUNT_LIMIT: Usage = Usage {
    bytes: 1_000_000_000,
    files: 256,
    active: 16,
};

#[derive(Default)]
struct Usage {
    bytes: u64,
    files: usize,
    active: usize,
}

impl Usage {
    fn add(&mut self, bytes: u64, active: bool) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.files += 1;
        self.active += usize::from(active);
    }
    fn admits(&self, size: u64, limit: &Self) -> bool {
        self.bytes.saturating_add(size) <= limit.bytes
            && self.files < limit.files
            && self.active < limit.active
    }
}

pub(super) fn lock(root: &Path, uid: u32) -> io::Result<File> {
    let lock = open_append(&root.join(".quota.lock"), uid).map_err(private_path_error)?;
    lock.lock()?;
    Ok(lock)
}

fn remove_empty_directory(path: &Path, legacy: bool) -> io::Result<()> {
    // Old processes do not take our account lock. Leave their freshly created
    // transfer and session directories alone during a rolling upgrade.
    if legacy
        && !fs::symlink_metadata(path)?
            .modified()?
            .elapsed()
            .is_ok_and(|age| age >= RETENTION)
    {
        return Ok(());
    }
    if fs::read_dir(path)?.next().is_none() {
        fs::remove_dir(path)?;
    }
    Ok(())
}

/// Caller holds the account lock while scanning and reserving its next transfer.
pub(super) fn scan(
    root: &Path,
    uid: u32,
    request: Option<(SessionId, u64)>,
    legacy: bool,
) -> io::Result<()> {
    let mut account = Usage::default();
    let mut session = Usage::default();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<SessionId>().ok())
        else {
            continue;
        };
        let parent = entry.path();
        validate_directory(&parent, uid).map_err(private_path_error)?;
        for transfer in fs::read_dir(&parent)? {
            let transfer = transfer?;
            if !transfer.file_name().to_str().is_some_and(|name| {
                name.len() == TransferId::LENGTH * 2
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) {
                continue;
            }
            let directory = transfer.path();
            validate_directory(&directory, uid).map_err(private_path_error)?;
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let active = name == ".part";
                if !active
                    && name != "file"
                    && !name.strip_prefix("file.").is_some_and(|ext| {
                        !ext.is_empty()
                            && ext.len() <= MAX_UPLOAD_EXTENSION_BYTES
                            && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                    })
                {
                    continue;
                }
                let path = entry.path();
                // A user can change or replace their published file. Never follow links.
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let metadata = fs::symlink_metadata(&path)?;
                let stale = if active && !legacy {
                    let part = open_read(&path, uid).map_err(private_path_error)?;
                    match part.try_lock() {
                        Ok(()) => true, // The staging process exited before publication.
                        Err(std::fs::TryLockError::WouldBlock) => false,
                        Err(std::fs::TryLockError::Error(error)) => return Err(error),
                    }
                } else {
                    metadata
                        .modified()?
                        .elapsed()
                        .is_ok_and(|age| age >= RETENTION)
                };
                if stale && validate_regular_file(&path, uid).is_ok() {
                    fs::remove_file(path)?;
                } else {
                    account.add(metadata.len(), active);
                    if request.is_some_and(|(requested, _)| id == requested) {
                        session.add(metadata.len(), active);
                    }
                }
            }
            remove_empty_directory(&directory, legacy)?;
        }
        remove_empty_directory(&parent, legacy)?;
    }
    if let Some((_, size)) = request
        && (!account.admits(size, &ACCOUNT_LIMIT) || !session.admits(size, &SESSION_LIMIT))
    {
        return Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            "host upload quota reached",
        ));
    }
    Ok(())
}
