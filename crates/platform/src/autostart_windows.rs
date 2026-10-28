//! Opt-in per-user Windows login startup, without starting a daemon now.
use crate::user_state::UserPaths;
use std::path::Path;
use winsafe::{HKEY, RegistryValue, co};

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "VoyttyDaemon";
/// Login entry argument; startup detaches itself from the invoking console.
pub const FOREGROUND_ARGUMENT: &str = "--internal-daemon";
/// One current account's login registration.
pub struct Autostart;
impl Autostart {
    /// Resolves the current user's registration owner.
    pub fn current(_: &UserPaths) -> Result<Self, String> {
        Ok(Self)
    }
    /// Resolves cleanup even after persistent setup was removed.
    pub fn for_cleanup(_: &UserPaths) -> Option<Self> {
        Some(Self)
    }
    fn render(executable: &Path) -> Result<String, String> {
        let path = executable
            .to_str()
            .ok_or("executable path is not Unicode")?;
        if !executable.is_absolute() || path.contains('"') || !executable.is_file() {
            return Err("autostart requires an absolute executable file".into());
        }
        Ok(format!("\"{path}\" {FOREGROUND_ARGUMENT}"))
    }
    /// Registers this executable for the next login only.
    pub fn enable(&self, executable: &Path) -> Result<(), String> {
        let value = Self::render(executable)?;
        let (key, _) = HKEY::CURRENT_USER
            .RegCreateKeyEx(
                KEY,
                None,
                co::REG_OPTION::NON_VOLATILE,
                co::KEY::SET_VALUE | co::KEY::QUERY_VALUE,
                None,
            )
            .map_err(|e| e.to_string())?;
        match key.RegQueryValueEx(Some(VALUE)) {
            Ok(RegistryValue::Sz(existing)) if existing == value => return Ok(()),
            Err(co::ERROR::FILE_NOT_FOUND) => {}
            Ok(_) => return Err("refusing to replace a different autostart registration".into()),
            Err(error) => return Err(error.to_string()),
        }
        key.RegSetValueEx(Some(VALUE), RegistryValue::Sz(value))
            .map_err(|e| e.to_string())
    }
    /// Removes only voytty's named login value.
    pub fn disable(&self) -> Result<(), String> {
        let key = match HKEY::CURRENT_USER.RegOpenKeyEx(
            Some(KEY),
            co::REG_OPTION::NoValue,
            co::KEY::SET_VALUE | co::KEY::QUERY_VALUE,
        ) {
            Ok(key) => key,
            Err(co::ERROR::FILE_NOT_FOUND) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        match key.RegQueryValueEx(Some(VALUE)) {
            Ok(RegistryValue::Sz(value))
                if value == Self::render(&std::env::current_exe().map_err(|e| e.to_string())?)? => {
            }
            Err(co::ERROR::FILE_NOT_FOUND) => return Ok(()),
            Ok(_) => return Err("refusing to remove a different autostart registration".into()),
            Err(error) => return Err(error.to_string()),
        }
        match key.RegDeleteValue(Some(VALUE)) {
            Ok(()) | Err(co::ERROR::FILE_NOT_FOUND) => Ok(()),
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
            Some(KEY),
            co::REG_OPTION::NoValue,
            co::KEY::QUERY_VALUE,
        ) {
            Ok(key) => key,
            Err(co::ERROR::FILE_NOT_FOUND) => return Ok("Daemon autostart: disabled.\n".into()),
            Err(error) => return Err(error.to_string()),
        };
        match key.RegQueryValueEx(Some(VALUE)) {
            Ok(RegistryValue::Sz(value)) if value == Self::render(executable)? => {
                Ok("Daemon autostart: enabled (next login).\n".into())
            }
            Err(co::ERROR::FILE_NOT_FOUND) => Ok("Daemon autostart: disabled.\n".into()),
            Ok(_) => Err("autostart registration differs from this executable".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}
