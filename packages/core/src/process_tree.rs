#[cfg(unix)]
mod imp {
    pub(crate) struct ProcessTree {
        pid: Option<u32>,
    }

    impl ProcessTree {
        pub(crate) fn new(pid: Option<u32>) -> Self {
            Self { pid }
        }

        pub(crate) fn kill(&self) {
            if let Some(pid) = self.pid {
                // SAFETY: negative pid targets the process group the child leads.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }

        pub(crate) fn running(&mut self) -> bool {
            let Some(pid) = self.pid else {
                return false;
            };
            // SAFETY: signal 0 only checks whether the group still exists.
            let result = unsafe { libc::kill(-(pid as i32), 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                self.disarm();
                return false;
            }
            true
        }

        pub(crate) fn disarm(&mut self) {
            self.pid = None;
        }
    }

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            self.kill();
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JobObjectBasicAccountingInformation, QueryInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    const NO_INHERIT: i32 = 0;
    const TERMINATED: u32 = 1;

    struct JobObject(HANDLE);

    // SAFETY: a job object handle is a kernel handle usable from any thread.
    unsafe impl Send for JobObject {}
    // SAFETY: the owning guard closes the handle exactly once.
    unsafe impl Sync for JobObject {}

    impl JobObject {
        fn create() -> Option<Self> {
            // SAFETY: a null name and null security attributes request a private job object.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            (!handle.is_null()).then_some(Self(handle))
        }

        fn assign(&self, process: HANDLE) -> bool {
            // SAFETY: both handles are live for the duration of the call.
            unsafe { AssignProcessToJobObject(self.0, process) != 0 }
        }

        fn active_processes(&self) -> Option<u32> {
            let mut information = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            let size = size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32;
            // SAFETY: the buffer matches the requested information class.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut information).cast(),
                    size,
                    std::ptr::null_mut(),
                )
            };
            (ok != 0).then_some(information.ActiveProcesses)
        }

        fn terminate(&self) {
            // SAFETY: the job handle stays live until the guard is dropped.
            unsafe {
                TerminateJobObject(self.0, TERMINATED);
            }
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            // SAFETY: this guard owns the handle and closes it exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub(crate) struct ProcessTree {
        pid: Option<u32>,
        job: Option<JobObject>,
    }

    impl ProcessTree {
        pub(crate) fn new(pid: Option<u32>) -> Self {
            let Some(pid) = pid else {
                return Self {
                    pid: None,
                    job: None,
                };
            };
            let Some(job) = JobObject::create() else {
                crate::log::key(format_args!("[oaklet] could not create a job object"));
                return Self {
                    pid: Some(pid),
                    job: None,
                };
            };
            // SAFETY: these are the rights AssignProcessToJobObject needs.
            let process =
                unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, NO_INHERIT, pid) };
            if process.is_null() {
                crate::log::key(format_args!(
                    "[oaklet] could not open process {pid} to assign it to a job object"
                ));
                return Self {
                    pid: Some(pid),
                    job: None,
                };
            }
            let assigned = job.assign(process);
            // SAFETY: this handle was created here and is not used afterwards.
            unsafe {
                CloseHandle(process);
            }
            if !assigned {
                crate::log::key(format_args!(
                    "[oaklet] could not assign process {pid} to a job object; stopping it will not reach its children"
                ));
                return Self {
                    pid: Some(pid),
                    job: None,
                };
            }
            Self {
                pid: Some(pid),
                job: Some(job),
            }
        }

        pub(crate) fn kill(&self) {
            if let Some(job) = &self.job {
                job.terminate();
                return;
            }
            if let Some(pid) = self.pid {
                let _ = std::process::Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        pub(crate) fn running(&mut self) -> bool {
            let Some(job) = &self.job else {
                return false;
            };
            if job.active_processes().is_some_and(|active| active > 0) {
                return true;
            }
            self.disarm();
            false
        }

        pub(crate) fn disarm(&mut self) {
            self.job = None;
            self.pid = None;
        }
    }

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            self.kill();
        }
    }
}

pub(crate) use imp::ProcessTree;
