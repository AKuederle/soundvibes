use std::env;
use std::fmt;
use std::io::Read as _;
use std::path::Path;
use std::process::Command;

use crate::types::InjectBackend;

/// Known terminal emulator window classes (lowercase for comparison)
const TERMINAL_CLASSES: &[&str] = &[
    "konsole",
    "org.kde.konsole",
    "kitty",
    "alacritty",
    "gnome-terminal",
    "org.gnome.terminal",
    "xterm",
    "urxvt",
    "terminator",
    "tilix",
    "xfce4-terminal",
    "mate-terminal",
    "lxterminal",
    "st",
    "foot",
    "wezterm",
    "com.mitchellh.ghostty",
    "ghostty",
];

#[derive(Debug)]
pub struct OutputError {
    message: String,
}

impl OutputError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub fn inject_text(text: &str, backend: InjectBackend) -> Result<(), OutputError> {
    match backend {
        InjectBackend::Ydotool => {
            if let Some(err) = try_ydotool(text)? {
                Err(OutputError::new(err))
            } else {
                Ok(())
            }
        }
        InjectBackend::Wtype => {
            if let Some(err) = try_wayland(text)? {
                Err(OutputError::new(err))
            } else {
                Ok(())
            }
        }
        InjectBackend::Xdotool => {
            if let Some(err) = try_x11(text)? {
                Err(OutputError::new(err))
            } else {
                Ok(())
            }
        }
        InjectBackend::Auto => inject_text_auto(text),
    }
}

fn inject_text_auto(text: &str) -> Result<(), OutputError> {
    let mut errors = Vec::new();

    // Try clipboard paste first - instant and avoids modifier key conflicts
    if let Some(err) = try_clipboard_paste(text)? {
        errors.push(err);
    } else {
        return Ok(());
    }

    // Try ydotool - works on both Wayland and X11 via kernel uinput
    if let Some(err) = try_ydotool(text)? {
        errors.push(err);
    } else {
        return Ok(());
    }

    // Try Wayland backend
    if let Some(err) = try_wayland(text)? {
        errors.push(err);
    } else {
        return Ok(());
    }

    // Try X11 backend
    if let Some(err) = try_x11(text)? {
        errors.push(err);
    } else {
        return Ok(());
    }

    Err(OutputError::new(format!(
        "no supported injection backends available ({})",
        errors.join("; ")
    )))
}

/// Copy text to clipboard with Klipper-hidden hint using wl-clipboard-rs.
/// Offers both text/plain and x-kde-passwordManagerHint MIME types so Klipper
/// skips recording this entry in its history.
fn clipboard_copy_secret(text: &str) -> Result<(), String> {
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

    let sources = vec![
        MimeSource {
            source: Source::Bytes(text.as_bytes().into()),
            mime_type: MimeType::Text,
        },
        MimeSource {
            source: Source::Bytes(b"secret"[..].into()),
            mime_type: MimeType::Specific("x-kde-passwordManagerHint".to_string()),
        },
    ];

    Options::new()
        .copy_multi(sources)
        .map_err(|e| format!("clipboard copy failed: {e}"))
}

/// Save current clipboard contents (returns None if clipboard is empty).
fn clipboard_save() -> Option<Vec<u8>> {
    use wl_clipboard_rs::paste;

    let (mut reader, _mime) = paste::get_contents(
        paste::ClipboardType::Regular,
        paste::Seat::Unspecified,
        paste::MimeType::Any,
    )
    .ok()?;

    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Restore previously saved clipboard contents (with secret hint so Klipper
/// doesn't record the restore as a new history entry).
fn clipboard_restore(data: &[u8]) -> Result<(), String> {
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

    let sources = vec![
        MimeSource {
            source: Source::Bytes(data.into()),
            mime_type: MimeType::Text,
        },
        MimeSource {
            source: Source::Bytes(b"secret"[..].into()),
            mime_type: MimeType::Specific("x-kde-passwordManagerHint".to_string()),
        },
    ];

    Options::new()
        .copy_multi(sources)
        .map_err(|e| format!("clipboard restore failed: {e}"))
}

/// Clear the clipboard.
fn clipboard_clear() -> Result<(), String> {
    use wl_clipboard_rs::copy::{self, ClipboardType, Seat};

    copy::clear(ClipboardType::Regular, Seat::All)
        .map_err(|e| format!("clipboard clear failed: {e}"))
}

/// Try clipboard paste: copy to clipboard, then simulate Ctrl+V or Ctrl+Shift+V
fn try_clipboard_paste(text: &str) -> Result<Option<String>, OutputError> {
    if !has_ydotool() {
        return Ok(Some(
            "clipboard paste requires ydotool for key simulation".to_string()
        ));
    }

    // Save current clipboard contents
    let previous_clipboard = clipboard_save();

    // Copy text to clipboard with Klipper-hidden hint
    if let Err(msg) = clipboard_copy_secret(text) {
        return Ok(Some(msg));
    }

    // Detect if focused window is a terminal
    let is_terminal = is_focused_window_terminal();

    // Simulate paste: Ctrl+V for normal apps, Ctrl+Shift+V for terminals
    // Key codes: 29=LCtrl, 42=LShift, 47=V
    let key_sequence = if is_terminal {
        // Ctrl+Shift+V: Ctrl down, Shift down, V down, V up, Shift up, Ctrl up
        vec!["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]
    } else {
        // Ctrl+V: Ctrl down, V down, V up, Ctrl up
        vec!["29:1", "47:1", "47:0", "29:0"]
    };

    let args: Vec<&str> = std::iter::once("key")
        .chain(key_sequence.into_iter())
        .collect();

    let result = match Command::new("ydotool").args(&args).status() {
        Ok(status) if status.success() => Ok(None),
        Ok(status) => Ok(Some(format!("ydotool exited with status {status}"))),
        Err(e) => Ok(Some(format!("failed to run ydotool: {e}"))),
    };

    // Give the target application time to read the clipboard before restoring
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Restore previous clipboard contents or clear
    if let Some(prev) = &previous_clipboard {
        let _ = clipboard_restore(prev);
    } else {
        let _ = clipboard_clear();
    }

    result
}

/// Check if the currently focused window is a terminal emulator
fn is_focused_window_terminal() -> bool {
    // Try kdotool (KDE Wayland)
    if let Ok(output) = Command::new("kdotool")
        .args(["getactivewindow", "getwindowclassname"])
        .output()
    {
        if output.status.success() {
            let class = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
            return TERMINAL_CLASSES.iter().any(|t| class.contains(t));
        }
    }

    // Try xdotool (X11)
    if let Ok(output) = Command::new("xdotool")
        .args(["getactivewindow", "getwindowclassname"])
        .output()
    {
        if output.status.success() {
            let class = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
            return TERMINAL_CLASSES.iter().any(|t| class.contains(t));
        }
    }

    // Can't detect, assume not terminal (use Ctrl+V)
    false
}

fn try_ydotool(text: &str) -> Result<Option<String>, OutputError> {
    if !has_ydotool() {
        return Ok(Some(
            "ydotoold not running; start with `systemctl --user start ydotool.service` \
             (see README for uinput permissions setup)".to_string()
        ));
    }

    match run_command(
        "ydotool",
        &["type", "-d", "0", "--", text],
        "install ydotool and run `systemctl --user start ydotool.service`",
    ) {
        Ok(()) => Ok(None),
        Err(err) => Ok(Some(format!("ydotool: {err}"))),
    }
}

fn has_ydotool() -> bool {
    let uid = unsafe { libc::getuid() };
    let socket_paths = [
        format!("/run/user/{}/.ydotool_socket", uid),
        "/tmp/.ydotool_socket".to_string(),
    ];
    socket_paths.iter().any(|p| Path::new(p).exists())
}

fn try_wayland(text: &str) -> Result<Option<String>, OutputError> {
    if !has_wayland_session() {
        return Ok(Some("wayland session not detected".to_string()));
    }

    match run_command(
        "wtype",
        &["--", text],
        "install wtype to enable Wayland text injection",
    ) {
        Ok(()) => Ok(None),
        Err(err) => Ok(Some(format!("wayland: {err}"))),
    }
}

fn try_x11(text: &str) -> Result<Option<String>, OutputError> {
    if !has_x11_session() {
        return Ok(Some("x11 session not detected".to_string()));
    }

    match run_command(
        "xdotool",
        &["type", "--clearmodifiers", "--delay", "0", "--", text],
        "install xdotool to enable X11 text injection",
    ) {
        Ok(()) => Ok(None),
        Err(err) => Ok(Some(format!("x11: {err}"))),
    }
}

fn has_wayland_session() -> bool {
    if let Ok(value) = env::var("XDG_SESSION_TYPE") {
        if value.eq_ignore_ascii_case("wayland") {
            return true;
        }
    }
    env::var_os("WAYLAND_DISPLAY").is_some()
}

fn has_x11_session() -> bool {
    if let Ok(value) = env::var("XDG_SESSION_TYPE") {
        if value.eq_ignore_ascii_case("x11") {
            return true;
        }
    }
    env::var_os("DISPLAY").is_some()
}

fn run_command(program: &str, args: &[&str], help: &str) -> Result<(), OutputError> {
    let status = Command::new(program).args(args).status().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            OutputError::new(format!("{program} not found; {help}"))
        } else {
            OutputError::new(format!("failed to run {program}: {err}"))
        }
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(OutputError::new(format!(
            "{program} exited with status {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn detects_wayland_session_from_env() {
        let _guard = EnvGuard::set("XDG_SESSION_TYPE", "wayland");
        assert!(has_wayland_session());
    }

    #[test]
    fn detects_x11_session_from_env() {
        let _guard = EnvGuard::set("XDG_SESSION_TYPE", "x11");
        assert!(has_x11_session());
    }

    #[test]
    fn detects_wayland_session_from_display_fallback() {
        let _guard = EnvGuard::remove("XDG_SESSION_TYPE");
        let _wayland_guard = EnvGuard::set("WAYLAND_DISPLAY", "wayland-0");
        let _display_guard = EnvGuard::remove("DISPLAY");
        assert!(has_wayland_session());
    }
}
