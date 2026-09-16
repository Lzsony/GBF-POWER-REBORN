#![cfg(windows)]
use gbf_core::windows_process::Child;
use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
#[ignore = "subprocess fixture for quoted_startup_does_not_launch_prefix_executable"]
fn quoted_path_fixture() {
    let exe = std::env::current_exe().unwrap();
    // Only a copied fixture inside a test-created directory may write a marker.
    if exe
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with("gbf-quote-"))
    {
        fs::write(exe.with_extension("started"), b"started").unwrap();
    }
}

#[test]
fn quoted_startup_does_not_launch_prefix_executable() {
    use std::os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::{Foundation::*, System::Threading::*};
    let dir = tempfile::Builder::new()
        .prefix("gbf-quote-")
        .tempdir()
        .unwrap();
    let app = dir.path().join("GBF App 中文");
    fs::create_dir(&app).unwrap();
    let intended = app.join("reborn.exe");
    let prefix = dir.path().join("GBF.exe");
    fs::copy(std::env::current_exe().unwrap(), &intended).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &prefix).unwrap();
    let mut line = gbf_core::windows_process::quote(intended.as_os_str());
    line.push(" --exact quoted_path_fixture --ignored");
    let mut line: Vec<_> = line.encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        // Null application name reproduces Windows Run command interpretation.
        assert_ne!(
            CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_NO_WINDOW,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut info
            ),
            0
        );
        let process = OwnedHandle::from_raw_handle(info.hProcess);
        let _thread = OwnedHandle::from_raw_handle(info.hThread);
        let waited = WaitForSingleObject(process.as_raw_handle(), 5000);
        if waited != WAIT_OBJECT_0 {
            TerminateProcess(process.as_raw_handle(), 1);
        }
        assert_eq!(waited, WAIT_OBJECT_0);
    }
    assert!(intended.with_extension("started").exists());
    assert!(!prefix.with_extension("started").exists());
}

fn command(root: &Path, role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "process_fixture", "--ignored", "--nocapture"])
        .env("GBF_PROCESS_TEST_ROOT", root)
        .env("GBF_PROCESS_TEST_ROLE", role)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "process barrier timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "subprocess fixture; invoked by job_owns_descendants_before_execution"]
fn process_fixture() {
    let Ok(root) = std::env::var("GBF_PROCESS_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let role = std::env::var("GBF_PROCESS_TEST_ROLE").unwrap();
    if role == "parent" {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let child = Child::spawn(&command(root, "child")).unwrap();
            until(|| root.join("stop").exists());
            drop(child);
        });
    } else {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let record = format!(
            "{} {}",
            std::process::id(),
            listener.local_addr().unwrap().port()
        );
        let pending = root.join(format!("{role}.pending"));
        fs::write(&pending, record).unwrap();
        fs::rename(pending, root.join(&role)).unwrap();
        let _grandchild = (role == "child").then(|| command(root, "grandchild").spawn().unwrap());
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
#[test]
fn job_owns_descendants_before_execution() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{Foundation::*, System::Threading::*};
    struct Cleanup {
        parent: std::process::Child,
        descendants: Vec<OwnedHandle>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.parent.kill();
            let _ = self.parent.wait();
            // Retain handles from the launch barrier: PID reuse cannot target another process.
            for handle in &self.descendants {
                unsafe {
                    TerminateProcess(handle.as_raw_handle(), 1);
                }
            }
        }
    }
    for crash in [false, true] {
        let dir = tempfile::Builder::new()
            .prefix("GBF 程序 tree ")
            .tempdir()
            .unwrap();
        let mut cleanup = Cleanup {
            parent: command(dir.path(), "parent").spawn().unwrap(),
            descendants: vec![],
        };
        let mut ports = vec![];
        for role in ["child", "grandchild"] {
            let path = dir.path().join(role);
            until(|| path.exists());
            let record = fs::read_to_string(path).unwrap();
            let values: Vec<_> = record.split_whitespace().collect();
            let id = values[0].parse().unwrap();
            let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, id) };
            assert!(!handle.is_null());
            cleanup
                .descendants
                .push(unsafe { OwnedHandle::from_raw_handle(handle) });
            ports.push(values[1].parse::<u16>().unwrap());
        }
        if crash {
            cleanup.parent.kill().unwrap();
        } else {
            fs::write(dir.path().join("stop"), b"stop").unwrap();
        }
        until(|| cleanup.parent.try_wait().unwrap().is_some());
        for port in ports {
            until(|| TcpListener::bind(("127.0.0.1", port)).is_ok());
        }
        for handle in &cleanup.descendants {
            let result = unsafe { WaitForSingleObject(handle.as_raw_handle(), 2000) };
            assert_eq!(result, WAIT_OBJECT_0, "owned descendant is still running");
        }
        cleanup.descendants.clear();
    }
}
