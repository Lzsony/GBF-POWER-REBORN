//! Native Security.framework hooks supplied by both the app and its extension.
use keyring::{credential::CredentialApi, Error, Result};
use std::{
    any::Any,
    ffi::{c_char, CStr, CString},
};

unsafe extern "C" {
    fn gpr_ios_secret_read(name: *const c_char, status: *mut i32) -> *mut c_char;
    fn gpr_ios_secret_write(name: *const c_char, value: *const c_char) -> i32;
    fn gpr_ios_secret_delete(name: *const c_char) -> i32;
    fn gpr_ios_secret_free(value: *mut c_char);
    fn gpr_ios_certificate_trusted(pem: *const c_char, leaf: *const u8, length: usize) -> bool;
}
#[derive(Debug)]
struct Credential {
    name: CString,
}
fn failure() -> Error {
    Error::PlatformFailure(Box::new(std::io::Error::other(
        "iOS Keychain operation failed",
    )))
}
impl CredentialApi for Credential {
    fn set_secret(&self, value: &[u8]) -> Result<()> {
        self.set_password(std::str::from_utf8(value).map_err(|_| failure())?)
    }
    fn get_secret(&self) -> Result<Vec<u8>> {
        self.get_password().map(String::into_bytes)
    }
    fn set_password(&self, value: &str) -> Result<()> {
        let value = CString::new(value).map_err(|_| failure())?;
        if unsafe { gpr_ios_secret_write(self.name.as_ptr(), value.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(failure())
        }
    }
    fn get_password(&self) -> Result<String> {
        let mut status = -1;
        let value = unsafe { gpr_ios_secret_read(self.name.as_ptr(), &mut status) };
        if value.is_null() {
            return Err(if status == 1 {
                Error::NoEntry
            } else {
                failure()
            });
        }
        let result = unsafe { CStr::from_ptr(value) }
            .to_str()
            .map(str::to_owned)
            .map_err(|_| failure());
        unsafe {
            gpr_ios_secret_free(value);
        }
        result
    }
    fn delete_credential(&self) -> Result<()> {
        match unsafe { gpr_ios_secret_delete(self.name.as_ptr()) } {
            0 => Ok(()),
            1 => Err(Error::NoEntry),
            _ => Err(failure()),
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn debug_fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("iOS shared Keychain credential")
    }
}
pub fn entry(name: &str) -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new_with_credential(Box::new(Credential {
        name: CString::new(name)?,
    })))
}
pub fn certificate_trusted(pem: &str, leaf: &[u8]) -> anyhow::Result<bool> {
    let pem = CString::new(pem)?;
    Ok(unsafe { gpr_ios_certificate_trusted(pem.as_ptr(), leaf.as_ptr(), leaf.len()) })
}
