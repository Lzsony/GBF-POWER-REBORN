//! Data directories. Call only after acquiring the single-instance guard.
use anyhow::{bail, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const NAME: &str = "GBF Power Reborn";

pub(crate) fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

fn directory(path: PathBuf) -> Result<PathBuf> {
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_dir() || is_link(&meta) => {
            bail!("Data must be a regular directory: {}", path.display())
        }
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(&path)?,
        Err(e) => return Err(e.into()),
    }
    Ok(path)
}
pub fn runtime_directory(root: &Path) -> Result<PathBuf> {
    directory(root.to_path_buf())?;
    directory(root.join("runtime"))
}
pub fn windows_root(local: &Path) -> Result<PathBuf> {
    directory(local.join(NAME))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_directories_preserve_data_on_reopen() {
        let local = tempfile::tempdir().unwrap();
        let root = windows_root(local.path()).unwrap();
        let runtime = runtime_directory(&root).unwrap();
        fs::write(runtime.join("state"), "keep").unwrap();
        assert_eq!(runtime_directory(&root).unwrap(), runtime);
        assert_eq!(fs::read_to_string(runtime.join("state")).unwrap(), "keep");
    }
    #[cfg(windows)]
    #[test]
    fn junction_boundaries_preserve_external_data() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let body = external.path().join(format!("{}.body", "a".repeat(64)));
        fs::write(&body, "outside data").unwrap();
        for name in ["runtime", "ja", "en"] {
            let link = root.path().join(name);
            let output = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&link)
                .arg(external.path())
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(is_link(&fs::symlink_metadata(&link).unwrap()));
            if name != "runtime" {
                assert!(crate::cache::Cache::open(root.path()).is_err());
            } else {
                assert!(runtime_directory(root.path()).is_err());
            }
            assert_eq!(fs::read_to_string(&body).unwrap(), "outside data");
            // Remove only this verified junction, never its target directory.
            fs::remove_dir(&link).unwrap();
        }
    }
}
