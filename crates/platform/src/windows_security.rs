//! Windows SID and filesystem ACL boundary. Numeric UIDs are never authorization.
use std::{fs, io, os::windows::fs::MetadataExt, path::Path};
use win_custody::{ObjectSecurityDescriptor, OwnedSid, SecurityDescriptor};

pub(crate) fn sid() -> io::Result<String> {
    win_custody::current_user_sid_string()
}

pub(crate) fn private_sddl() -> io::Result<String> {
    Ok(format!("D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)", sid()?))
}

fn file_security(path: &Path) -> io::Result<ObjectSecurityDescriptor> {
    use std::os::windows::fs::OpenOptionsExt;
    // query_file requests only the DACL. Query the file's kernel handle to
    // retrieve both OWNER_SECURITY_INFORMATION and DACL_SECURITY_INFORMATION.
    // READ_CONTROL needs no data access; BACKUP_SEMANTICS permits directories.
    let file = fs::OpenOptions::new()
        .read(true)
        .access_mode(0x0002_0000)
        .custom_flags(0x0200_0000 | 0x0020_0000)
        .open(path)?;
    if file.metadata()?.file_attributes() & 0x400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed path is a reparse point",
        ));
    }
    ObjectSecurityDescriptor::query_kernel_object(&file)
}

pub(crate) fn validate(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.file_attributes() & 0x400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed path is a reparse point",
        ));
    }
    let descriptor = file_security(path)?;
    let user = OwnedSid::from_string(&sid()?)?;
    let system = OwnedSid::from_string("S-1-5-18")?;
    let administrators = OwnedSid::from_string("S-1-5-32-544")?;
    let owner = descriptor.owner().map_err(io::Error::other)?;
    // Elevated creation can assign the Administrators owner. It remains in
    // the OS trust boundary when the same user later runs unelevated; the
    // DACL must still admit this full user SID and no other ordinary account.
    if !owner.matches(&user) && !owner.matches(&administrators) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed path belongs to another Windows account",
        ));
    }
    let dacl = descriptor.dacl().map_err(io::Error::other)?;
    if !dacl
        .allowed_aces()
        .any(|ace| ace.trustee_matches(&user) && ace.flags() & 0x08 == 0)
        || dacl
            .allowed_aces()
            .any(|ace| !ace.trustee_matches(&user) && !ace.trustee_matches(&system))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed path grants access to another Windows account",
        ));
    }
    Ok(())
}

pub(crate) fn create_directory(path: &Path) -> io::Result<()> {
    // Publish only after applying the private DACL, so concurrent startup
    // never observes an existing but not-yet-secured managed directory.
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("directory has no parent"))?;
    validate_parent(parent)?;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staged = parent.join(format!(
        ".voytty.directory-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir(&staged)?;
    let result = (|| {
        SecurityDescriptor::from_sddl(&private_sddl()?)?.apply_to_tree(&staged, true)?;
        validate(&staged)?;
        // Windows does not replace an existing directory during rename.
        fs::rename(&staged, path).map_err(|error| {
            if fs::symlink_metadata(path).is_ok() {
                io::Error::from(io::ErrorKind::AlreadyExists)
            } else {
                error
            }
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir(&staged);
    }
    result
}

pub(crate) fn authorize_process(pid: u32) -> io::Result<()> {
    use winsafe::{HPROCESS, TokenInfo, co};
    let process = HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, pid)
        .map_err(io::Error::other)?;
    let token = process
        .OpenProcessToken(co::TOKEN::QUERY)
        .map_err(io::Error::other)?;
    let info = token
        .GetTokenInformation(co::TOKEN_INFORMATION_CLASS::User)
        .map_err(io::Error::other)?;
    if let TokenInfo::User(user) = info
        && let Some(peer) = user.User.Sid()
        && peer.to_string() == sid()?
    {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "local pipe peer belongs to another Windows account",
    ))
}

pub(crate) fn protect_new_file(path: &Path) -> io::Result<()> {
    SecurityDescriptor::from_sddl(&private_sddl()?)?.apply_to_tree(path, true)?;
    validate(path)
}

pub(crate) fn remove_retired_executable(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => return Ok(()),
        Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => {}
        Err(error) => return Err(error),
    }
    // Windows image mappings survive renames. A separate system process waits
    // for the invoking frontend/helper to exit, then deletes only this exact
    // retired file. The path is data in a child-only environment variable.
    use std::os::windows::process::CommandExt;
    let powershell =
        std::path::PathBuf::from(winsafe::GetSystemDirectory().map_err(io::Error::other)?)
            .join("WindowsPowerShell/v1.0/powershell.exe");
    let script = "$p = $env:VOYTTY_RETIRED_EXECUTABLE; $end = [DateTime]::UtcNow.AddMinutes(2); do { try { [IO.File]::Delete($p); exit 0 } catch [IO.IOException] { Start-Sleep -Milliseconds 100 } catch [UnauthorizedAccessException] { Start-Sleep -Milliseconds 100 } } while ([DateTime]::UtcNow -lt $end); exit 1";
    let child = std::process::Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("VOYTTY_RETIRED_EXECUTABLE", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x08000000)
        .spawn()?;
    drop(child);
    Ok(())
}

/// New objects may be secured after creation only inside a trusted parent.
/// Administrators/SYSTEM are part of the OS trust boundary; ordinary users
/// must have no mutation rights, including deleting or replacing children.
pub(crate) fn validate_parent(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "managed parent traverses a reparse point",
            ));
        }
    }
    let descriptor = file_security(path)?;
    let trusted = [
        OwnedSid::from_string(&sid()?)?,
        OwnedSid::from_string("S-1-5-18")?,
        OwnedSid::from_string("S-1-5-32-544")?,
    ];
    let owner = descriptor.owner().map_err(io::Error::other)?;
    if !trusted.iter().any(|sid| owner.matches(sid)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "untrusted managed parent owner",
        ));
    }
    // GENERIC_ALL/WRITE, DELETE, WRITE_DAC/OWNER, directory add/delete children,
    // and write attributes/extended attributes. Inherit-only ACEs do not grant
    // access to the parent itself.
    const MUTATION: u32 = 0x500D_0156;
    if descriptor
        .dacl()
        .map_err(io::Error::other)?
        .allowed_aces()
        .any(|ace| {
            ace.flags() & 0x08 == 0
                && ace.mask() & MUTATION != 0
                && !trusted.iter().any(|sid| ace.trustee_matches(sid))
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed parent is writable by another account",
        ));
    }
    Ok(())
}
