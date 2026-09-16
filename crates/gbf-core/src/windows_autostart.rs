//! Own only our two HKCU values, retaining exact values for rollback.
use anyhow::{Context, Result};
use std::{ffi::OsStr, io, path::Path};
use winreg::{enums::*, types::ToRegValue, RegKey, RegValue};

const PATHS: [&str; 2] = [
    r"Software\Microsoft\Windows\CurrentVersion\Run",
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
];
#[derive(Debug, PartialEq)]
pub struct Snapshot([Option<RegValue>; 2]);
fn copy_value(value: &RegValue) -> RegValue {
    RegValue {
        bytes: value.bytes.clone(),
        vtype: value.vtype.clone(),
    }
}
impl Clone for Snapshot {
    fn clone(&self) -> Self {
        Self(std::array::from_fn(|i| self.0[i].as_ref().map(copy_value)))
    }
}
impl Snapshot {
    pub fn enabled(&self) -> bool {
        self.0[0].is_some()
            && self.0[1].as_ref().is_none_or(|value| {
                value.vtype == REG_BINARY
                    && value.bytes.len() >= 12
                    && matches!(value.bytes[0], 2 | 6)
            })
    }
}
trait Registry {
    fn read(&self, slot: usize) -> io::Result<Option<RegValue>>;
    fn write(&self, slot: usize, value: Option<&RegValue>) -> io::Result<()>;
}
pub struct Registration {
    name: String,
}
impl Registration {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
    /// Test harness cleanup is restricted to our explicit test registration names.
    pub fn remove_test_registration(&self) -> Result<()> {
        anyhow::ensure!(
            self.name == "GBF Native Lifecycle Test"
                || self.name == "GBF Internal Test"
                || self.name.starts_with("GBF-Reborn-Test-"),
            "not a test registration"
        );
        restore(self, &Snapshot([None, None]))
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        snapshot(self)
    }
    pub fn restore(&self, old: &Snapshot) -> Result<()> {
        restore(self, old)
    }
    pub fn set(&self, enabled: bool, executable: &Path, args: &[&OsStr]) -> Result<()> {
        change(self, enabled, executable, args, false)
    }
    pub fn reconcile(&self, executable: &Path, args: &[&OsStr]) -> Result<bool> {
        let enabled = self.snapshot()?.enabled();
        if enabled {
            change(self, true, executable, args, true)?;
        }
        Ok(enabled)
    }
}
impl Registry for Registration {
    fn read(&self, slot: usize) -> io::Result<Option<RegValue>> {
        match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(PATHS[slot])
            .and_then(|k| k.get_raw_value(&self.name))
        {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn write(&self, slot: usize, value: Option<&RegValue>) -> io::Result<()> {
        if let Some(value) = value {
            let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(PATHS[slot])?;
            key.set_raw_value(&self.name, value)
        } else {
            match RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(PATHS[slot], KEY_SET_VALUE)
                .and_then(|key| key.delete_value(&self.name))
            {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }
}
fn snapshot(registry: &impl Registry) -> Result<Snapshot> {
    Ok(Snapshot([registry.read(0)?, registry.read(1)?]))
}
fn restore(registry: &impl Registry, old: &Snapshot) -> Result<()> {
    // Attempt both restorations even if the first fails.
    let first = registry.write(0, old.0[0].as_ref());
    let second = registry.write(1, old.0[1].as_ref());
    first.context("autostart Run rollback failed")?;
    second.context("autostart approval rollback failed")?;
    Ok(())
}
fn change(
    registry: &impl Registry,
    enabled: bool,
    executable: &Path,
    args: &[&OsStr],
    preserve_approval: bool,
) -> Result<()> {
    let old = snapshot(registry)?;
    let mut next = old.clone();
    if enabled {
        anyhow::ensure!(
            executable.is_absolute(),
            "autostart requires an absolute executable path"
        );
        let mut line = crate::windows_process::quote(executable.as_os_str());
        for arg in args {
            line.push(" ");
            line.push(crate::windows_process::quote(arg));
        }
        next.0[0] = Some(line.to_reg_value());
        if !preserve_approval {
            next.0[1] = Some(RegValue {
                vtype: REG_BINARY,
                bytes: vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            });
        }
    } else {
        next.0[0] = None;
        // Disabling removes registration; retain the system's approval decision.
    }
    for slot in 0..2 {
        if next.0[slot] != old.0[slot] {
            if let Err(error) = registry.write(slot, next.0[slot].as_ref()) {
                restore(registry, &old).context("autostart update and rollback failed")?;
                return Err(error.into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Fake {
        values: RefCell<Snapshot>,
        fail: Cell<Option<usize>>,
    }
    impl Registry for Fake {
        fn read(&self, slot: usize) -> io::Result<Option<RegValue>> {
            Ok(self.values.borrow().0[slot].as_ref().map(copy_value))
        }
        fn write(&self, slot: usize, value: Option<&RegValue>) -> io::Result<()> {
            if self.fail.get() == Some(slot) {
                self.fail.set(None);
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            self.values.borrow_mut().0[slot] = value.map(copy_value);
            Ok(())
        }
    }
    #[test]
    fn quoting_move_disabled_and_exact_rollback() {
        let fake = Fake {
            values: RefCell::new(Snapshot([None, None])),
            fail: Cell::new(None),
        };
        let a = Path::new(r"C:\中文 A\reborn.exe");
        let b = Path::new(r"C:\中文 B\reborn.exe");
        change(&fake, true, a, &[OsStr::new("--resume-proxy")], false).unwrap();
        let old = snapshot(&fake).unwrap();
        assert!(old.enabled());
        assert_eq!(
            old.0[0],
            Some(OsStr::new("\"C:\\中文 A\\reborn.exe\" \"--resume-proxy\"").to_reg_value())
        );
        change(&fake, true, b, &[], true).unwrap();
        assert_ne!(snapshot(&fake).unwrap().0[0], old.0[0]);
        let mut disabled = old.clone();
        disabled.0[1].as_mut().unwrap().bytes[0] = 3;
        restore(&fake, &disabled).unwrap();
        assert!(!snapshot(&fake).unwrap().enabled());
        fake.fail.set(Some(1));
        assert!(change(&fake, true, b, &[], false).is_err());
        assert_eq!(snapshot(&fake).unwrap(), disabled);
        change(&fake, true, b, &[], false).unwrap();
        assert!(snapshot(&fake).unwrap().enabled());
    }
    #[test]
    fn isolated_native_registration_moves_without_reenabling_disabled_entry() {
        let registration = Registration::new(format!(
            "GBF-Reborn-Test-{}-{}",
            std::process::id(),
            rand_core::OsRng.next_u64()
        ));
        use rand_core::RngCore;
        let old = registration.snapshot().unwrap();
        struct Cleanup<'a>(&'a Registration, Snapshot);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                self.0.restore(&self.1).unwrap();
            }
        }
        let _cleanup = Cleanup(&registration, old);
        let a = Path::new(r"C:\中文 A\reborn.exe");
        let b = Path::new(r"C:\中文 B\reborn.exe");
        registration.set(true, a, &[]).unwrap();
        assert!(registration.reconcile(b, &[]).unwrap());
        assert_eq!(
            registration.snapshot().unwrap().0[0],
            Some(crate::windows_process::quote(b.as_os_str()).to_reg_value())
        );
        let mut disabled = registration.snapshot().unwrap();
        disabled.0[1].as_mut().unwrap().bytes[0] = 3;
        registration.restore(&disabled).unwrap();
        assert!(!registration.reconcile(a, &[]).unwrap());
        assert_eq!(registration.snapshot().unwrap(), disabled);
    }
}
