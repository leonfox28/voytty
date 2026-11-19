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

// Published paths belong to the user, who may move them during an inventory.
fn if_present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn managed_entries(path: &Path, uid: u32) -> io::Result<Option<fs::ReadDir>> {
    if let Err(error) = validate_directory(path, uid) {
        if if_present(fs::symlink_metadata(path))?.is_none() {
            return Ok(None);
        }
        return Err(private_path_error(error));
    }
    if_present(fs::read_dir(path))
}

fn retire_directory(path: &Path, removed_file: bool) -> io::Result<()> {
    // Leave freshly emptied directories for callers removing their own files.
    let Some(metadata) = if_present(fs::symlink_metadata(path))? else {
        return Ok(());
    };
    if !removed_file
        && !metadata
            .modified()?
            .elapsed()
            .is_ok_and(|age| age >= RETENTION)
    {
        return Ok(());
    }
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
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
        let Some(transfers) = managed_entries(&parent, uid)? else {
            continue;
        };
        for transfer in transfers {
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
            let Some(files) = managed_entries(&directory, uid)? else {
                continue;
            };
            let mut removed_file = false;
            for entry in files {
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
                let Some(metadata) = if_present(fs::symlink_metadata(&path))? else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
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
                    removed_file |= if_present(fs::remove_file(path))?.is_some();
                } else {
                    account.add(metadata.len(), active);
                    if request.is_some_and(|(requested, _)| id == requested) {
                        session.add(metadata.len(), active);
                    }
                }
            }
            retire_directory(&directory, removed_file)?;
        }
        retire_directory(&parent, false)?;
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
