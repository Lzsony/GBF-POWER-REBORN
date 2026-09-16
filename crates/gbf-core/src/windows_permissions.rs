//! Keep OpenSSH device keys private to the current Windows account.
use crate::error::ErrorCode;
use anyhow::{bail, Context, Result};
use std::{
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

fn powershell(path: &Path, script: &str) -> Result<()> {
    let executable =
        Path::new(&std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = Command::new(executable)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("GBF_PRIVATE_PATH", path)
        // PowerShell 7's module paths cannot be loaded by Windows PowerShell 5.1.
        .env_remove("PSModulePath")
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context(ErrorCode::SshNotConfigured)?;
    if !output.status.success() {
        #[cfg(test)]
        eprintln!(
            "ACL test operation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        bail!(ErrorCode::SshNotConfigured);
    }
    Ok(())
}

pub(crate) fn protect_directory(path: &Path) -> Result<()> {
    powershell(
        path,
        r#"
$ErrorActionPreference='Stop'
$sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl=New-Object System.Security.AccessControl.DirectorySecurity
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true,$false)
$rule=New-Object System.Security.AccessControl.FileSystemAccessRule($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow')
$acl.AddAccessRule($rule)
[IO.Directory]::SetAccessControl($env:GBF_PRIVATE_PATH,$acl)
"#,
    )
}

pub(crate) fn verify_key(path: &Path) -> Result<()> {
    powershell(
        path,
        r#"
$ErrorActionPreference='Stop'
$sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User
$item=Get-Item -LiteralPath $env:GBF_PRIVATE_PATH -Force
if($item.Attributes -band [IO.FileAttributes]::ReparsePoint){exit 1}
$acl=Get-Acl -LiteralPath $env:GBF_PRIVATE_PATH
if($acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $sid.Value){exit 1}
$rules=$acl.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])
$readable=$false
foreach($rule in $rules){
  if($rule.AccessControlType -eq 'Allow'){
    if($rule.IdentityReference.Value -notin @($sid.Value,'S-1-5-18','S-1-5-32-544')){exit 1}
    if($rule.IdentityReference.Value -eq $sid.Value -and ($rule.FileSystemRights -band [System.Security.AccessControl.FileSystemRights]::ReadData)){$readable=$true}
  }
}
if(-not $readable){exit 1}
"#,
    )
}

pub(crate) fn protect_new_key(path: &Path) -> Result<()> {
    powershell(
        path,
        r#"
$ErrorActionPreference='Stop'
$sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl=New-Object System.Security.AccessControl.FileSecurity
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true,$false)
$acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($sid,'FullControl','Allow')))
[IO.File]::SetAccessControl($env:GBF_PRIVATE_PATH,$acl)
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_key_acl_rejects_other_users() {
        let dir = tempfile::tempdir().unwrap();
        protect_directory(dir.path()).unwrap();
        let key = dir.path().join("test key.txt");
        std::fs::write(&key, "not a real private key").unwrap();
        protect_new_key(&key).unwrap();
        verify_key(&key).unwrap();
        powershell(&key, r#"
$ErrorActionPreference='Stop'
$acl=New-Object System.Security.AccessControl.FileSecurity
$user=[System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl.SetOwner($user)
$acl.SetAccessRuleProtection($true,$false)
$acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($user,'FullControl','Allow')))
$sid=New-Object System.Security.Principal.SecurityIdentifier('S-1-1-0')
$acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($sid,'Read','Allow')))
[IO.File]::SetAccessControl($env:GBF_PRIVATE_PATH,$acl)
"#).unwrap();
        assert!(verify_key(&key).is_err());
    }
}
