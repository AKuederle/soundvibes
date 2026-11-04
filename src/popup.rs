//! Native history UI runs in a child process; clipboard writes stay in the daemon.
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::error::AppError;

#[derive(Serialize, Deserialize)]
pub struct PopupContent {
    pub title: String,
    pub transcripts: Vec<String>,
    pub x: i32,
    pub y: i32,
}

unsafe extern "C" {
    fn sv_history_popup(json: *const u8, len: usize, inspect: bool, select: i32) -> i32;
}

/// Internal child entry point. Qt owns this process's main thread.
pub fn run(inspect: bool, select: i32) -> Result<(), AppError> {
    let content: PopupContent = serde_json::from_reader(std::io::stdin())
        .map_err(|err| AppError::runtime(format!("invalid history popup input: {err}")))?;
    let json = serde_json::to_vec(&content)
        .map_err(|err| AppError::runtime(format!("failed to encode popup input: {err}")))?;
    // Qt reads the borrowed bytes during this call and retains no Rust pointers.
    let selected = unsafe { sv_history_popup(json.as_ptr(), json.len(), inspect, select) };
    if selected < 0 {
        return Err(AppError::runtime("history popup could not read its input"));
    }
    if !inspect {
        println!(
            "{}",
            serde_json::to_string(&selected.checked_sub(1).filter(|_| selected > 0))
                .map_err(|err| AppError::runtime(err.to_string()))?
        );
    }
    Ok(())
}

pub(crate) fn show(
    content: &PopupContent,
    token: Option<String>,
) -> Result<Option<usize>, AppError> {
    let executable = std::env::current_exe()
        .map_err(|err| AppError::runtime(format!("failed to locate sv: {err}")))?;
    let mut command = Command::new(executable);
    command
        .arg("tray-popup")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(token) = token {
        command.env("XDG_ACTIVATION_TOKEN", token);
    }
    // Close the popup if its daemon is restarted while the window is open.
    // The child executes only async-signal-safe syscalls before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|err| AppError::runtime(format!("failed to open history popup: {err}")))?;
    let input = serde_json::to_vec(content).map_err(|err| AppError::runtime(err.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(err) = stdin.write_all(&input) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::runtime(format!(
                "failed to send history to popup: {err}"
            )));
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| AppError::runtime(format!("failed to wait for popup: {err}")))?;
    if !output.status.success() {
        return Err(AppError::runtime(format!(
            "history popup exited with {}",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| AppError::runtime(format!("invalid popup selection: {err}")))
}
