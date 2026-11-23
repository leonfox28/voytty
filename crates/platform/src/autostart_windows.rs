//! Opt-in per-user Windows login startup, without starting a daemon now.
use crate::user_state::UserPaths;
use std::path::{Path, PathBuf};
use winsafe::{HKEY, RegistryValue, co};

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "VoyttyDaemon";
const OWNER_KEY: &str = r"Software\Voytty";
const OWNER_VALUE: &str = "LoginCommands";
/// Login entry argument; startup detaches itself from the invoking console.
pub const FOREGROUND_ARGUMENT: &str = "--internal-daemon";
/// One current account's login registration.
pub struct Autostart {
    paths: UserPaths,
    run_key: String,
    owner_key: String,
}
impl Autostart {
    /// Resolves the current user's registration owner.
    pub fn current(paths: &UserPaths) -> Result<Self, String> {
        Ok(Self::registration(paths))
    }
    /// Resolves cleanup even after persistent setup was removed.
    pub fn for_cleanup(paths: &UserPaths) -> Option<Self> {
        Some(Self::registration(paths))
    }
    fn registration(paths: &UserPaths) -> Self {
        Self {
            paths: paths.clone(),
            run_key: KEY.into(),
            owner_key: OWNER_KEY.into(),
        }
    }
    fn validate_executable(&self, executable: &Path) -> Result<(), String> {
        Self::command(executable)?;
        crate::user_state::validate_owned_executable(executable, self.paths.uid())
            .map_err(|error| format!("autostart: {error}"))
    }

    fn command(executable: &Path) -> Result<String, String> {
        let path = executable
            .to_str()
            .filter(|text| !text.chars().any(char::is_control))
            .ok_or(
                "autostart requires an absolute Unicode executable path without control characters",
            )?;
        if !executable.is_absolute()
            || path.contains('"')
            || path.ends_with('\\')
            || executable
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err("autostart requires an absolute executable file".into());
        }
        Ok(format!("\"{path}\" {FOREGROUND_ARGUMENT}"))
    }
    // Exact, separately stored ownership survives relocation without claiming a foreign Run value.
    fn owns(&self, existing: &str, current: &Path) -> Result<bool, String> {
        if existing == Self::command(current)? {
            return Ok(true);
        }
        match HKEY::CURRENT_USER.RegGetValue(
            Some(&self.owner_key),
            Some(OWNER_VALUE),
            co::RRF::RT_REG_MULTI_SZ,
        ) {
            Ok(RegistryValue::MultiSz(recorded)) => {
                return Ok(recorded.iter().any(|command| command == existing));
            }
            Err(co::ERROR::FILE_NOT_FOUND) => {}
            Err(error) => return Err(error.to_string()),
            Ok(_) => return Ok(false),
        }
        // Registrations predating LoginCommands use the executable recorded by setup or update.
        let metadata = self.paths.install_metadata();
        if !metadata.try_exists().map_err(|e| e.to_string())? {
            return Ok(false);
        }
        crate::user_state::validate_regular_file(metadata, self.paths.uid())
            .map_err(|e| e.to_string())?;
        #[derive(serde::Deserialize)]
        struct Installation {
            executable: PathBuf,
        }
        let installed: Installation =
            serde_json::from_reader(std::fs::File::open(metadata).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        Ok(existing == Self::command(&installed.executable)?)
    }

    fn forget_owner(&self) -> Result<(), String> {
        let key = match HKEY::CURRENT_USER.RegOpenKeyEx(
            Some(&self.owner_key),
            co::REG_OPTION::NoValue,
            co::KEY::SET_VALUE,
        ) {
            Ok(key) => key,
            Err(co::ERROR::FILE_NOT_FOUND) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        match key.RegDeleteValue(Some(OWNER_VALUE)) {
            Ok(()) | Err(co::ERROR::FILE_NOT_FOUND) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Registers this executable for the next login only.
    pub fn enable(&self, executable: &Path) -> Result<(), String> {
        self.validate_executable(executable)?;
        let value = Self::command(executable)?;
        let (key, _) = HKEY::CURRENT_USER
            .RegCreateKeyEx(
                &self.run_key,
                None,
                co::REG_OPTION::NON_VOLATILE,
                co::KEY::SET_VALUE | co::KEY::QUERY_VALUE,
                None,
            )
            .map_err(|e| e.to_string())?;
        let mut commands = vec![value.clone()];
        match key.RegQueryValueEx(Some(VALUE)) {
            Ok(RegistryValue::Sz(existing)) if self.owns(&existing, executable)? => {
                if existing != value {
                    commands.push(existing);
                }
            }
            Err(co::ERROR::FILE_NOT_FOUND) => {}
            Ok(_) => return Err("refusing to replace a different autostart registration".into()),
            Err(error) => return Err(error.to_string()),
        }
        // Either command is recoverable if the process exits between the two registry writes.
        HKEY::CURRENT_USER
            .RegSetKeyValue(
                Some(&self.owner_key),
                Some(OWNER_VALUE),
                RegistryValue::MultiSz(commands),
            )
            .map_err(|e| e.to_string())?;
        key.RegSetValueEx(Some(VALUE), RegistryValue::Sz(value))
            .map_err(|e| e.to_string())
    }
    /// Removes only voytty's named login value.
    pub fn disable(&self) -> Result<(), String> {
        let key = match HKEY::CURRENT_USER.RegOpenKeyEx(
            Some(&self.run_key),
            co::REG_OPTION::NoValue,
            co::KEY::SET_VALUE | co::KEY::QUERY_VALUE,
        ) {
            Ok(key) => key,
            Err(co::ERROR::FILE_NOT_FOUND) => return self.forget_owner(),
            Err(error) => return Err(error.to_string()),
        };
        match key.RegQueryValueEx(Some(VALUE)) {
            Ok(RegistryValue::Sz(value))
                if self.owns(&value, &std::env::current_exe().map_err(|e| e.to_string())?)? => {}
            Err(co::ERROR::FILE_NOT_FOUND) => return self.forget_owner(),
            Ok(_) => return Err("refusing to remove a different autostart registration".into()),
            Err(error) => return Err(error.to_string()),
        }
        match key.RegDeleteValue(Some(VALUE)) {
            Ok(()) | Err(co::ERROR::FILE_NOT_FOUND) => self.forget_owner(),
            Err(e) => Err(e.to_string()),
        }
    }
    /// Removes login registration after an explicit stop/reset/uninstall.
    pub fn remove_after_stop(&self) -> Result<(), String> {
        self.disable()
    }
    /// Inspects registration without starting or changing anything.
    pub fn status(&self, executable: &Path) -> Result<String, String> {
        let key = match HKEY::CURRENT_USER.RegOpenKeyEx(
            Some(&self.run_key),
            co::REG_OPTION::NoValue,
            co::KEY::QUERY_VALUE,
        ) {
            Ok(key) => key,
            Err(co::ERROR::FILE_NOT_FOUND) => return Ok("Daemon autostart: disabled.\n".into()),
            Err(error) => return Err(error.to_string()),
        };
        match key.RegQueryValueEx(Some(VALUE)) {
            Ok(RegistryValue::Sz(value)) if value == Self::command(executable)? => {
                self.validate_executable(executable)?;
                Ok("Daemon autostart: enabled (next login).\n".into())
            }
            Err(co::ERROR::FILE_NOT_FOUND) => Ok("Daemon autostart: disabled.\n".into()),
            Ok(RegistryValue::Sz(value)) if self.owns(&value, executable)? => {
                Ok("Daemon autostart: points to a previous location. Run `voytty daemon autostart enable` to update it.\n".into())
            }
            Ok(_) => Err("autostart registration differs from this executable".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_owned_executable(path: &Path) {
        std::fs::write(path, b"fixture").expect("executable fixture");
        crate::windows_security::protect_new_file(path).expect("private executable ACL");
    }

    #[test]
    fn command_rejects_unsafe_paths_and_control_characters() {
        for invalid in [
            Path::new("relative\\voytty.exe"),
            Path::new(r#"C:\Users\alice\bad"name.exe"#),
            Path::new("C:\\Users\\alice\\bad\nname.exe"),
            Path::new("C:\\Users\\alice\\bad\rname.exe"),
            Path::new("C:\\Users\\alice\\bad\tname.exe"),
            Path::new("C:\\Users\\alice\\bad\0name.exe"),
            Path::new(r"C:\Users\alice\trailing\"),
            Path::new(r"C:\Users\alice\..\voytty.exe"),
        ] {
            assert!(
                Autostart::command(invalid).is_err(),
                "must reject unsafe path {invalid:?}"
            );
        }
        assert_eq!(
            Autostart::command(Path::new(r"C:\Users\alice\voytty.exe")).expect("valid command"),
            r#""C:\Users\alice\voytty.exe" --internal-daemon"#
        );
    }

    #[test]
    fn moved_registrations_recover_from_legacy_metadata_and_preserve_foreign_values() {
        let dir = tempfile::tempdir().expect("fixture");
        let root = dir.path();
        let paths = UserPaths::for_test(0, root.into(), root.join("state"), root.join("runtime"));
        paths.prepare_state_directories().expect("private fixture");
        let namespace = format!(
            r"Software\VoyttyTests\{}",
            root.file_name().expect("unique fixture").to_string_lossy()
        );
        let registration = Autostart {
            paths: paths.clone(),
            run_key: format!(r"{namespace}\Run"),
            owner_key: format!(r"{namespace}\Owner"),
        };
        let old = root.join("old location.exe");
        let new = root.join("moved.exe");
        write_owned_executable(&new);
        let metadata = serde_json::json!({ "executable": old });
        crate::user_state::atomic_write(paths.install_metadata(), 0, |f| {
            f.write_all(metadata.to_string().as_bytes())
        })
        .expect("legacy metadata");
        let reg = |value: String| {
            HKEY::CURRENT_USER
                .RegSetKeyValue(
                    Some(&registration.run_key),
                    Some(VALUE),
                    RegistryValue::Sz(value),
                )
                .expect("test Run entry")
        };
        reg(Autostart::command(&old).expect("old command"));
        registration
            .enable(&new)
            .expect("repair older registration");
        assert!(
            registration
                .status(&new)
                .expect("status")
                .contains("enabled")
        );
        std::fs::remove_file(paths.install_metadata()).expect("no migration fallback");
        std::fs::remove_file(&new).expect("simulate another move");
        registration
            .disable()
            .expect("remove by recorded ownership");
        reg(r#""C:\Foreign\tool.exe" --internal-daemon"#.into());
        assert!(registration.disable().is_err());
        write_owned_executable(&new);
        assert!(registration.enable(&new).is_err());
        HKEY::CURRENT_USER
            .RegDeleteTree(Some(&namespace))
            .expect("remove private registry fixture");
    }

    #[test]
    fn enable_and_status_reject_untrusted_acl_directory_and_junction_targets() {
        let dir = tempfile::tempdir().expect("fixture");
        let root = dir.path();
        let paths = UserPaths::for_test(0, root.into(), root.join("state"), root.join("runtime"));
        paths.prepare_state_directories().expect("private fixture");
        let namespace = format!(
            r"Software\VoyttyTests\{}-security",
            root.file_name().expect("unique fixture").to_string_lossy()
        );
        let registration = Autostart {
            paths: paths.clone(),
            run_key: format!(r"{namespace}\Run"),
            owner_key: format!(r"{namespace}\Owner"),
        };
        let sid = win_custody::current_user_sid_string().expect("SID");

        let directory = root.join("not-a-file");
        std::fs::create_dir(&directory).expect("directory target");
        assert!(registration.enable(&directory).is_err());

        let executable = root.join("voytty.exe");
        write_owned_executable(&executable);
        win_custody::SecurityDescriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})(A;;FR;;;WD)"))
            .expect("public ACL")
            .apply_to_tree(&executable, true)
            .expect("apply untrusted ACL");
        assert!(
            registration.enable(&executable).is_err(),
            "enable must reject executable readable by another Windows account"
        );

        crate::windows_security::protect_new_file(&executable).expect("restore private ACL");
        registration
            .enable(&executable)
            .expect("enable private executable");
        assert!(
            registration
                .status(&executable)
                .expect("enabled status")
                .contains("enabled")
        );
        win_custody::SecurityDescriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})(A;;FR;;;WD)"))
            .expect("public ACL")
            .apply_to_tree(&executable, true)
            .expect("weaken ACL after enable");
        assert!(
            registration.status(&executable).is_err(),
            "status must revalidate executable ownership and ACL"
        );

        let real_bin = root.join("real-bin");
        std::fs::create_dir(&real_bin).expect("real bin dir");
        let real_exe = real_bin.join("voytty.exe");
        write_owned_executable(&real_exe);
        let junction = root.join("junction-bin");
        let mklink = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real_bin)
            .output()
            .expect("create NTFS directory junction");
        assert!(
            mklink.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&mklink.stderr)
        );
        let via_junction = junction.join("voytty.exe");
        assert!(
            registration.enable(&via_junction).is_err(),
            "enable must reject executable behind a reparse point"
        );
        let _ = std::fs::remove_dir(&junction);
        HKEY::CURRENT_USER
            .RegDeleteTree(Some(&namespace))
            .expect("remove private registry fixture");
    }
}
