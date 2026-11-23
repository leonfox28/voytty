//! Private effective-user upload staging. Only explicit successful publication persists.

#[path = "upload_quota.rs"]
mod quota;

#[cfg(unix)]
use crate::user_state::PathError;
use crate::user_state::{create_new_file, create_or_validate_directory, validate_directory};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::OnceLock;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
};
use voytty_core::{
    SessionId,
    upload::{TransferId, UploadMetadata, UploadedFile},
};

#[cfg(unix)]
static UNIX_FALLBACK_ROOT: OnceLock<(PathBuf, String)> = OnceLock::new();

/// One unique staging directory. Drop removes only an incomplete attempt.
pub struct StagedUpload {
    file: Option<File>,
    root: PathBuf,
    uid: u32,
    directory: PathBuf,
    public_path: String,
    metadata: UploadMetadata,
    accepted: u64,
    committed: bool,
}

impl StagedUpload {
    /// Creates private directories beneath the system `/tmp` alias, not `$TMPDIR`.
    pub fn create(
        session: SessionId,
        id: TransferId,
        metadata: UploadMetadata,
    ) -> io::Result<Self> {
        let (root, public, uid) = Self::location()?;
        Self::create_at(root, public, uid, session, id, metadata)
    }

    /// Reclaims abandoned staging and expired files, including after a restart.
    pub fn clean_expired() -> io::Result<()> {
        let (root, _, uid) = Self::location()?;
        if root.try_exists()? {
            validate_directory(&root, uid).map_err(private_path_error)?;
            let _lock = quota::lock(&root, uid)?;
            quota::scan(&root, uid, None, false)?;
        }
        #[cfg(unix)]
        {
            if let Some((fallback_root, _)) = UNIX_FALLBACK_ROOT.get()
                && fallback_root != &root
                && fallback_root.try_exists()?
            {
                validate_directory(fallback_root, uid).map_err(private_path_error)?;
                let _lock = quota::lock(fallback_root, uid)?;
                quota::scan(fallback_root, uid, None, false)?;
            }
            // Older daemons used the fallback runtime directory. Only collect
            // canonical transfer names after the retention period on that tree.
            let legacy = fs::canonicalize("/tmp")?.join(format!("voytty-{uid}"));
            if legacy.try_exists()? && validate_directory(&legacy, uid).is_ok() {
                // Runtime cleanup owns the old root and must never see a new control file.
                quota::scan(&legacy, uid, None, true)?;
            }
        }
        Ok(())
    }

    fn location() -> io::Result<(PathBuf, String, u32)> {
        #[cfg(windows)]
        {
            let account = crate::account::EffectiveAccount::current().map_err(io::Error::other)?;
            let root = account
                .home()
                .join(format!("voytty-upload-{}", crate::windows_security::sid()?));
            let public = root.to_str().ok_or_else(invalid_bytes)?.replace('\\', "/");
            Ok((root, public, account.uid()))
        }
        #[cfg(unix)]
        {
            let uid = nix::unistd::geteuid().as_raw();
            let base = fs::canonicalize("/tmp")?;
            let (root, public) = resolve_unix_root(&base, "/tmp", uid, &UNIX_FALLBACK_ROOT)?;
            Ok((root, public, uid))
        }
    }

    fn create_at(
        root: PathBuf,
        public_root: String,
        uid: u32,
        session: SessionId,
        id: TransferId,
        metadata: UploadMetadata,
    ) -> io::Result<Self> {
        create_or_validate_directory(&root, uid).map_err(private_path_error)?;
        let _lock = quota::lock(&root, uid)?;
        quota::scan(&root, uid, Some((session, metadata.size())), false)?;
        let session_name = session.to_string();
        let parent = root.join(&session_name);
        create_or_validate_directory(&parent, uid).map_err(private_path_error)?;
        let transfer_name: String = id
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let directory = parent.join(&transfer_name);
        // A collision never reuses or removes another transfer's directory.
        #[cfg(unix)]
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        #[cfg(windows)]
        crate::windows_security::create_directory(&directory)?;
        let created = (|| {
            validate_directory(&directory, uid).map_err(private_path_error)?;
            let file = create_new_file(&directory.join(".part")).map_err(private_path_error)?;
            file.lock()?;
            // Reserve the declared size before releasing the account lock.
            file.set_len(metadata.size())?;
            Ok(file)
        })();
        let file = match created {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_file(directory.join(".part"));
                let _ = fs::remove_dir(&directory);
                return Err(error);
            }
        };
        let filename = if metadata.extension().is_empty() {
            "file".into()
        } else {
            format!("file.{}", metadata.extension())
        };
        Ok(Self {
            file: Some(file),
            root,
            uid,
            directory,
            public_path: format!("{public_root}/{session_name}/{transfer_name}/{filename}"),
            metadata,
            accepted: 0,
            committed: false,
        })
    }

    /// Writes only exact sequential bytes within the declared size.
    pub fn write_chunk(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(invalid_bytes)?;
        if offset != self.accepted || end > self.metadata.size() || data.is_empty() {
            return Err(invalid_bytes());
        }
        self.file
            .as_mut()
            .ok_or_else(invalid_bytes)?
            .write_all(data)?;
        self.accepted = end;
        Ok(())
    }

    /// Accepted file bytes, independently counted from incoming metadata.
    #[must_use]
    pub const fn accepted_bytes(&self) -> u64 {
        self.accepted
    }

    /// Syncs and publishes one complete file. Drop never removes it afterwards.
    pub fn publish(mut self) -> io::Result<UploadedFile> {
        if self.accepted != self.metadata.size() {
            return Err(invalid_bytes());
        }
        let result = UploadedFile::new(self.public_path.clone(), self.accepted)
            .map_err(|_| invalid_bytes())?;
        self.file.as_ref().ok_or_else(invalid_bytes)?.sync_all()?;
        let _lock = quota::lock(&self.root, self.uid)?;
        drop(self.file.take());
        let filename = self
            .public_path
            .rsplit('/')
            .next()
            .ok_or_else(invalid_bytes)?;
        fs::rename(self.directory.join(".part"), self.directory.join(filename))?;
        // From this point, even a directory-sync failure has an uncertain committed outcome.
        self.committed = true;
        crate::user_state::sync_directory(&self.directory).map_err(private_path_error)?;
        Ok(result)
    }
}

impl Drop for StagedUpload {
    fn drop(&mut self) {
        if !self.committed {
            let Ok(_lock) = quota::lock(&self.root, self.uid) else {
                return;
            };
            drop(self.file.take());
            let _ = fs::remove_file(self.directory.join(".part"));
            let _ = fs::remove_dir(&self.directory);
            if let Some(session) = self.directory.parent() {
                let _ = fs::remove_dir(session);
            }
        }
    }
}

#[cfg(unix)]
fn resolve_unix_root(
    base_dir: &Path,
    public_base: &str,
    uid: u32,
    fallback_cache: &OnceLock<(PathBuf, String)>,
) -> io::Result<(PathBuf, String)> {
    let primary_name = format!("voytty-upload-{uid}");
    let primary = base_dir.join(&primary_name);
    match create_or_validate_directory(&primary, uid) {
        Ok(()) => Ok((primary, format!("{public_base}/{primary_name}"))),
        Err(
            PathError::WrongOwner { .. }
            | PathError::WrongType(_)
            | PathError::Symlink(_)
            | PathError::WrongMode { .. },
        ) => {
            if let Some((cached, public)) = fallback_cache.get()
                && create_or_validate_directory(cached, uid).is_ok()
            {
                return Ok((cached.clone(), public.clone()));
            }
            for _ in 0..8 {
                let name = format!("{primary_name}-{}", unix_root_nonce()?);
                let candidate = base_dir.join(&name);
                match fs::DirBuilder::new().mode(0o700).create(&candidate) {
                    Ok(()) => {
                        validate_directory(&candidate, uid).map_err(private_path_error)?;
                        let public = format!("{public_base}/{name}");
                        let _ = fallback_cache.set((candidate.clone(), public.clone()));
                        return Ok((candidate, public));
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unable to allocate private upload directory",
            ))
        }
        Err(error) => Err(private_path_error(error)),
    }
}

#[cfg(unix)]
fn unix_root_nonce() -> io::Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0_u8; 8];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("OS randomness unavailable"))?;
    Ok(format!("{:016x}", u64::from_be_bytes(bytes)))
}

fn invalid_bytes() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "upload byte count mismatch")
}
fn private_path_error(_: crate::user_state::PathError) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "upload directory failed ownership, type or mode validation",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    fn create(root: PathBuf, id: u8, size: u64) -> io::Result<StagedUpload> {
        StagedUpload::create_at(
            root,
            "/tmp/voytty-1".into(),
            nix::unistd::geteuid().as_raw(),
            SessionId::from_array([1; 16]),
            TransferId::from_array([id; 16]),
            UploadMetadata::new(size, "pdf").expect("upload fixture"),
        )
    }

    #[test]
    fn publication_keeps_exact_private_bytes_and_cancel_removes_only_staging() {
        let tmp = tempfile::tempdir().expect("upload fixture");
        let root = tmp.path().join("uploads");
        let mut staged = create(root.clone(), 2, 3).expect("upload fixture");
        let dir = staged.directory.clone();
        assert_eq!(
            fs::metadata(&dir).expect("upload fixture").mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.join(".part"))
                .expect("upload fixture")
                .mode()
                & 0o777,
            0o600
        );
        assert!(create(root.clone(), 2, 3).is_err());
        assert!(staged.write_chunk(1, b"abc").is_err());
        assert!(staged.write_chunk(0, b"abcd").is_err());
        staged.write_chunk(0, b"abc").expect("upload fixture");
        assert_eq!(staged.publish().expect("upload fixture").size(), 3);
        assert_eq!(
            fs::read(dir.join("file.pdf")).expect("upload fixture"),
            b"abc"
        );
        let cancelled = create(root.clone(), 3, 3).expect("upload fixture");
        let cancelled_dir = cancelled.directory.clone();
        drop(cancelled);
        assert!(!cancelled_dir.exists());
        assert!(dir.join("file.pdf").exists());
        assert_eq!(
            create(root, 4, 0)
                .expect("upload fixture")
                .publish()
                .expect("upload fixture")
                .size(),
            0
        );
    }

    #[test]
    fn active_reservations_release_capacity_and_expired_files_are_collected() -> io::Result<()> {
        use std::time::{Duration, SystemTime};
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("uploads");
        let mut active = (0..4)
            .map(|id| create(root.clone(), id, 1))
            .collect::<io::Result<Vec<_>>>()?;
        assert_eq!(
            create(root.clone(), 6, 1)
                .err()
                .expect("session concurrency limit")
                .kind(),
            io::ErrorKind::QuotaExceeded
        );
        active.pop();
        let finished = create(root.clone(), 6, 0)?;
        let path = finished.directory.join("file.pdf");
        finished.publish()?;
        File::open(&path)?.set_modified(SystemTime::now() - Duration::from_secs(25 * 3600))?;
        let _next = create(root, 7, 0)?;
        assert!(!path.exists());
        assert!(active[0].directory.join(".part").exists());
        Ok(())
    }

    #[test]
    fn published_files_count_against_session_and_account_byte_limits_after_reopen() -> io::Result<()>
    {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("uploads");
        let reserve = |session, transfer, size| {
            StagedUpload::create_at(
                root.clone(),
                "/tmp/voytty-1".into(),
                nix::unistd::geteuid().as_raw(),
                SessionId::from_array([session; 16]),
                TransferId::from_array([transfer; 16]),
                UploadMetadata::new(size, "").expect("valid fixture size"),
            )
        };
        // Sparse fixtures exercise on-disk accounting without writing 1 GB.
        for session in 0..5 {
            let staged = reserve(session, 0, 0)?;
            let path = staged.directory.join("file");
            staged.publish()?;
            File::options()
                .write(true)
                .open(path)?
                .set_len(200_000_000)?;
            assert_eq!(
                reserve(session, 1, 1)
                    .err()
                    .expect("session byte limit")
                    .kind(),
                io::ErrorKind::QuotaExceeded
            );
        }
        assert_eq!(
            reserve(9, 1, 1).err().expect("account byte limit").kind(),
            io::ErrorKind::QuotaExceeded
        );
        Ok(())
    }

    #[test]
    fn managed_symlinks_are_rejected() {
        let tmp = tempfile::tempdir().expect("upload fixture");
        let root = tmp.path().join("uploads");
        symlink(tmp.path(), &root).expect("upload fixture");
        assert!(create(root, 1, 1).is_err());
    }

    #[test]
    fn squatted_primary_unix_upload_root_falls_back_to_private_random_directory() {
        let tmp = tempfile::tempdir().expect("upload fixture");
        let uid = nix::unistd::geteuid().as_raw();
        let squatted = tmp.path().join(format!("voytty-upload-{uid}"));
        fs::write(&squatted, b"squatter").expect("squat file");
        let cache = OnceLock::new();
        let (fallback, public) =
            resolve_unix_root(tmp.path(), "/tmp", uid, &cache).expect("fallback root");
        assert_ne!(fallback, squatted);
        assert!(public.starts_with(&format!("/tmp/voytty-upload-{uid}-")));
        assert_eq!(
            fs::metadata(&fallback).expect("fallback meta").mode() & 0o777,
            0o700
        );
        let (reused, reused_public) =
            resolve_unix_root(tmp.path(), "/tmp", uid, &cache).expect("reused fallback root");
        assert_eq!(reused, fallback);
        assert_eq!(reused_public, public);
        assert!(
            UploadedFile::new(format!("{public}/abcd/ef01/file.pdf"), 1).is_ok(),
            "fallback upload path stays compatible with UploadedFile validation"
        );
    }
}
