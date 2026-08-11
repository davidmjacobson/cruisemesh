use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr, sync::mpsc, thread, time::Duration};

use anyhow::{bail, Context, Result};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE},
    System::{
        Console::FreeConsole,
        Power::{
            GetSystemPowerStatus, SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
            SYSTEM_POWER_STATUS,
        },
        Threading::CreateMutexW,
    },
    UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
};

const MUTEX_NAME: &str = "Local\\CruiseMeshHelperMutex";

pub struct SingleInstance(HANDLE);

impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        let name = wide(MUTEX_NAME);
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            bail!("CreateMutexW failed with Windows error {}", unsafe {
                GetLastError()
            });
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            bail!("CruiseMesh Helper is already running");
        }
        Ok(Self(handle))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub struct SleepGuard(Option<(mpsc::Sender<()>, thread::JoinHandle<()>)>);

impl SleepGuard {
    pub fn prevent_system_sleep(enabled: bool) -> Result<Self> {
        if !enabled {
            return Ok(Self(None));
        }
        let (stop, receive_stop) = mpsc::channel();
        let (ready, receive_ready) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("cruisemesh-power".into())
            .spawn(move || {
                let _ = ready.send(apply_power_policy());
                loop {
                    match receive_stop.recv_timeout(Duration::from_secs(30)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let _ = apply_power_policy();
                        }
                    }
                }
                unsafe {
                    SetThreadExecutionState(ES_CONTINUOUS);
                }
            })?;
        receive_ready
            .recv()
            .context("power-policy thread stopped during startup")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self(Some((stop, worker))))
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        if let Some((stop, worker)) = self.0.take() {
            let _ = stop.send(());
            let _ = worker.join();
        }
    }
}

fn apply_power_policy() -> std::result::Result<(), String> {
    let mut status = SYSTEM_POWER_STATUS {
        ACLineStatus: 0,
        BatteryFlag: 0,
        BatteryLifePercent: 0,
        SystemStatusFlag: 0,
        BatteryLifeTime: 0,
        BatteryFullLifeTime: 0,
    };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return Err(format!(
            "GetSystemPowerStatus failed with Windows error {}",
            unsafe { GetLastError() }
        ));
    }
    let flags = if status.ACLineStatus == 1 {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    if unsafe { SetThreadExecutionState(flags) } == 0 {
        return Err(format!(
            "SetThreadExecutionState failed with Windows error {}",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

pub fn install_logon_task(executable: &std::path::Path) -> Result<()> {
    let whoami = std::process::Command::new("whoami.exe")
        .output()
        .context("failed to identify the current Windows user")?;
    if !whoami.status.success() {
        bail!("whoami failed while creating the logon task");
    }
    let user = String::from_utf8(whoami.stdout)?.trim().to_owned();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RestartOnFailure><Interval>PT10S</Interval><Count>999</Count></RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author"><Exec><Command>{executable}</Command><Arguments>run</Arguments></Exec></Actions>
</Task>"#,
        user = xml_escape(&user),
        executable = xml_escape(&executable.display().to_string()),
    );
    let task_xml =
        std::env::temp_dir().join(format!("cruisemesh-helper-task-{}.xml", std::process::id()));
    std::fs::write(&task_xml, xml).context("failed to write Task Scheduler definition")?;
    let status = std::process::Command::new("schtasks.exe")
        .args([
            "/Create",
            "/F",
            "/TN",
            "CruiseMesh Helper",
            "/XML",
            &task_xml.to_string_lossy(),
        ])
        .status()
        .context("failed to launch Task Scheduler")?;
    let _ = std::fs::remove_file(task_xml);
    if !status.success() {
        bail!("Task Scheduler rejected the CruiseMesh logon task");
    }
    Ok(())
}

pub fn hide_console() {
    unsafe {
        FreeConsole();
    }
}

pub fn open_default_browser(url: &str) -> Result<()> {
    let operation = wide("open");
    let url = wide(url);
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        bail!(
            "Windows could not open the default browser (ShellExecute code {})",
            result as isize
        );
    }
    Ok(())
}

pub fn install_firewall_rule(executable: &std::path::Path) -> Result<()> {
    let program = format!("program={}", executable.display());
    let status = std::process::Command::new("netsh.exe")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=CruiseMesh Helper",
            "dir=in",
            "action=allow",
            &program,
            "protocol=TCP",
            "localport=45892",
            "profile=private,public",
            "enable=yes",
        ])
        .status()
        .context("failed to launch Windows Firewall configuration")?;
    if !status.success() {
        bail!("Windows Firewall rejected the CruiseMesh rule; run this command as administrator");
    }
    Ok(())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}
