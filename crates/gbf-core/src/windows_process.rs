//! Atomic Windows process-tree ownership. No child can run outside its Job.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    process::{Command, ExitStatus},
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::*,
    System::{JobObjects::*, Pipes::CreatePipe, Threading::*},
};

fn checked(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"));
    }
    value.push(0);
    Ok(value)
}
/// Windows CRT quoting, also used for the Run registry value.
pub fn quote(value: &OsStr) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    let mut out = vec![34];
    let mut slashes = 0;
    for c in value.encode_wide() {
        if c == 92 {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            92,
            slashes * if c == 34 { 2 } else { 1 },
        ));
        if c == 34 {
            out.push(92);
        }
        out.push(c);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(92, slashes * 2));
    out.push(34);
    OsString::from_wide(&out)
}

struct Attributes(Vec<usize>);
impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes);
        }
        let mut value = vec![0; bytes.div_ceil(size_of::<usize>())];
        unsafe {
            checked(InitializeProcThreadAttributeList(
                value.as_mut_ptr().cast(),
                2,
                0,
                &mut bytes,
            ))?;
        }
        Ok(Self(value))
    }
    fn set(&mut self, attribute: u32, handles: &[HANDLE]) -> io::Result<()> {
        unsafe {
            checked(UpdateProcThreadAttribute(
                self.0.as_mut_ptr().cast(),
                0,
                attribute as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                null_mut(),
                null(),
            ))
        }
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.0.as_mut_ptr().cast());
        }
    }
}

pub struct Child {
    job: OwnedHandle,
    process: OwnedHandle,
    pub stderr: Option<tokio::fs::File>,
}
impl Child {
    pub fn spawn(command: &Command) -> io::Result<Self> {
        Self::spawn_with_stderr(command, true)
    }
    pub fn spawn_quiet(command: &Command) -> io::Result<Self> {
        Self::spawn_with_stderr(command, false)
    }
    fn spawn_with_stderr(command: &Command, capture_stderr: bool) -> io::Result<Self> {
        // Every handle has a local owner before another fallible operation.
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = OwnedHandle::from_raw_handle(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            checked(SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            ))?;
            let security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let (mut read, mut write) = (null_mut(), null_mut());
            checked(CreatePipe(&mut read, &mut write, &security, 0))?;
            let read = OwnedHandle::from_raw_handle(read);
            let write = OwnedHandle::from_raw_handle(write);
            checked(SetHandleInformation(
                read.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                0,
            ))?;
            let nul = CreateFileW(
                wide(OsStr::new("NUL"))?.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &security,
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if nul == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let nul = OwnedHandle::from_raw_handle(nul);
            let mut attributes = Attributes::new()?;
            let jobs = [job.as_raw_handle()];
            let inherited = [write.as_raw_handle(), nul.as_raw_handle()];
            attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
            attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &inherited)?;
            let mut startup: STARTUPINFOEXW = zeroed();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = nul.as_raw_handle();
            startup.StartupInfo.hStdOutput = nul.as_raw_handle();
            startup.StartupInfo.hStdError = if capture_stderr {
                write.as_raw_handle()
            } else {
                nul.as_raw_handle()
            };
            startup.lpAttributeList = attributes.0.as_mut_ptr().cast();
            let mut line = quote(command.get_program());
            for arg in command.get_args() {
                line.push(" ");
                line.push(quote(arg));
            }
            let mut line = wide(&line)?;
            // Preserve inherited environment, applying explicit overrides case-insensitively.
            let mut environment: BTreeMap<String, (OsString, OsString)> = std::env::vars_os()
                .map(|(k, v)| (k.to_string_lossy().to_uppercase(), (k, v)))
                .collect();
            for (k, v) in command.get_envs() {
                let key = k.to_string_lossy().to_uppercase();
                if let Some(v) = v {
                    environment.insert(key, (k.into(), v.into()));
                } else {
                    environment.remove(&key);
                }
            }
            let mut block = Vec::new();
            for (_, (k, v)) in environment {
                let mut entry = k;
                entry.push("=");
                entry.push(v);
                block.extend(wide(&entry)?);
            }
            block.push(0);
            let directory = command
                .get_current_dir()
                .map(|p| wide(p.as_os_str()))
                .transpose()?;
            let mut info: PROCESS_INFORMATION = zeroed();
            // An explicit application name prevents same-prefix executable substitution.
            let mut executable = wide(command.get_program())?;
            if std::path::Path::new(command.get_program())
                .components()
                .count()
                == 1
            {
                let mut found = vec![0u16; 32768];
                let extension = wide(OsStr::new(".exe"))?;
                let count = SearchPathW(
                    null(),
                    executable.as_ptr(),
                    extension.as_ptr(),
                    found.len() as u32,
                    found.as_mut_ptr(),
                    null_mut(),
                );
                if count == 0 {
                    return Err(io::Error::last_os_error());
                }
                if count as usize >= found.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "executable path too long",
                    ));
                }
                found.truncate(count as usize + 1);
                executable = found;
            }
            checked(CreateProcessW(
                executable.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                block.as_ptr().cast(),
                directory.as_ref().map_or(null(), |p| p.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            ))?;
            let process = OwnedHandle::from_raw_handle(info.hProcess);
            let _thread = OwnedHandle::from_raw_handle(info.hThread);
            Ok(Self {
                job,
                process,
                stderr: capture_stderr
                    .then(|| tokio::fs::File::from_std(std::fs::File::from(read))),
            })
        }
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        unsafe {
            match WaitForSingleObject(self.process.as_raw_handle(), 0) {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    checked(GetExitCodeProcess(self.process.as_raw_handle(), &mut code))?;
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub async fn kill(&mut self) -> io::Result<()> {
        unsafe {
            checked(TerminateJobObject(self.job.as_raw_handle(), 1))?;
        }
        self.wait().await.map(|_| ())
    }
    pub fn terminate_and_wait(&mut self) -> io::Result<()> {
        unsafe {
            checked(TerminateJobObject(self.job.as_raw_handle(), 1))?;
            if WaitForSingleObject(self.process.as_raw_handle(), INFINITE) != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        // Closing the non-inherited Job handle is the crash path; explicit termination
        // also releases descendant-held stderr handles before readers are joined.
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}
