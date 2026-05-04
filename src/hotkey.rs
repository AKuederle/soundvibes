use std::collections::HashMap;
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use evdev::{Device, InputEventKind, Key};
use serde::Deserialize;

use crate::daemon::ControlEvent;
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub enabled: bool,
    pub key: Option<String>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            key: None,
        }
    }
}

pub fn start_listener(
    config: &HotkeyConfig,
    sender: Sender<ControlEvent>,
) -> Result<thread::JoinHandle<()>, AppError> {
    let key = config
        .key
        .as_deref()
        .ok_or_else(|| AppError::config("hotkey.key is required when hotkey.enabled is true"))?;
    let target = parse_key_name(key)?;
    let mut devices = open_keyboard_devices(Path::new("/dev/input"))?;
    if devices.is_empty() {
        return Err(AppError::runtime(
            "no keyboard device found in /dev/input; add your user to the input group or disable hotkey.enabled",
        ));
    }

    let handle = thread::spawn(move || {
        let mut pressed = false;
        loop {
            for device in &mut devices {
                match device.fetch_events() {
                    Ok(events) => {
                        for event in events {
                            if let InputEventKind::Key(key) = event.kind() {
                                if let Some(event) =
                                    hotkey_event(target, key, event.value(), &mut pressed)
                                {
                                    if sender.send(event).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(err) if err.raw_os_error() == Some(libc::ENODEV) => {}
                    Err(err) => eprintln!("hotkey device read error: {err}"),
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    Ok(handle)
}

fn open_keyboard_devices(input_dir: &Path) -> Result<Vec<Device>, AppError> {
    let entries = fs::read_dir(input_dir).map_err(|err| {
        AppError::runtime(format!(
            "failed to read {} for hotkey devices: {err}",
            input_dir.display()
        ))
    })?;
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_event_device(&path) {
            continue;
        }
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        if is_keyboard(&device) {
            set_nonblocking(&device);
            devices.push(device);
        }
    }
    Ok(devices)
}

fn is_event_device(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("event"))
        .unwrap_or(false)
}

fn is_keyboard(device: &Device) -> bool {
    device
        .supported_keys()
        .map(|keys| {
            keys.contains(Key::KEY_A) && keys.contains(Key::KEY_Z) && keys.contains(Key::KEY_ENTER)
        })
        .unwrap_or(false)
}

fn set_nonblocking(device: &Device) {
    let fd = device.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags != -1 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

fn hotkey_event(target: Key, key: Key, value: i32, pressed: &mut bool) -> Option<ControlEvent> {
    if key != target {
        return None;
    }
    match value {
        1 if !*pressed => {
            *pressed = true;
            Some(ControlEvent::StartRecording)
        }
        0 if *pressed => {
            *pressed = false;
            Some(ControlEvent::StopRecording)
        }
        2 => None,
        _ => None,
    }
}

pub fn parse_key_name(name: &str) -> Result<Key, AppError> {
    let trimmed = name.trim();
    if let Some(key) = parse_prefixed_keycode(trimmed)? {
        return Ok(key);
    }
    if trimmed.parse::<u16>().is_ok() || trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        return Err(AppError::config(format!(
            "bare numeric hotkey keycodes are ambiguous: {name}; use EVTEST_<code> or WEV_<code>"
        )));
    }

    let normalized = trimmed
        .chars()
        .map(|c| match c {
            '-' | ' ' => '_',
            c => c.to_ascii_uppercase(),
        })
        .collect::<String>();
    let key_name = normalized
        .strip_prefix("KEY_")
        .unwrap_or(&normalized)
        .to_string();

    common_key_names()
        .get(key_name.as_str())
        .copied()
        .ok_or_else(|| {
            AppError::config(format!(
                "unknown hotkey key '{name}'; use evtest to find a KEY_* name"
            ))
        })
}

fn common_key_names() -> HashMap<&'static str, Key> {
    [
        ("SCROLLLOCK", Key::KEY_SCROLLLOCK),
        ("PAUSE", Key::KEY_PAUSE),
        ("CAPSLOCK", Key::KEY_CAPSLOCK),
        ("INSERT", Key::KEY_INSERT),
        ("LEFTCTRL", Key::KEY_LEFTCTRL),
        ("LEFT_CTRL", Key::KEY_LEFTCTRL),
        ("LCTRL", Key::KEY_LEFTCTRL),
        ("RIGHTCTRL", Key::KEY_RIGHTCTRL),
        ("RIGHT_CTRL", Key::KEY_RIGHTCTRL),
        ("RCTRL", Key::KEY_RIGHTCTRL),
        ("LEFTALT", Key::KEY_LEFTALT),
        ("LALT", Key::KEY_LEFTALT),
        ("RIGHTALT", Key::KEY_RIGHTALT),
        ("RALT", Key::KEY_RIGHTALT),
        ("LEFTSHIFT", Key::KEY_LEFTSHIFT),
        ("RIGHTSHIFT", Key::KEY_RIGHTSHIFT),
        ("LEFTMETA", Key::KEY_LEFTMETA),
        ("RIGHTMETA", Key::KEY_RIGHTMETA),
        ("F13", Key::KEY_F13),
        ("F14", Key::KEY_F14),
        ("F15", Key::KEY_F15),
        ("F16", Key::KEY_F16),
        ("F17", Key::KEY_F17),
        ("F18", Key::KEY_F18),
        ("F19", Key::KEY_F19),
        ("F20", Key::KEY_F20),
        ("F21", Key::KEY_F21),
        ("F22", Key::KEY_F22),
        ("F23", Key::KEY_F23),
        ("F24", Key::KEY_F24),
        ("MEDIA", Key::KEY_MEDIA),
        ("RECORD", Key::KEY_RECORD),
        ("ESC", Key::KEY_ESC),
        ("ESCAPE", Key::KEY_ESC),
        ("SPACE", Key::KEY_SPACE),
    ]
    .into_iter()
    .collect()
}

const XKB_OFFSET: u16 = 8;

fn parse_prefixed_keycode(value: &str) -> Result<Option<Key>, AppError> {
    let normalized = value.to_ascii_uppercase();
    let (number, subtract_xkb_offset) = if let Some(number) = normalized.strip_prefix("WEV_") {
        (number, true)
    } else if let Some(number) = normalized.strip_prefix("XEV_") {
        (number, true)
    } else if let Some(number) = normalized.strip_prefix("X11_") {
        (number, true)
    } else if let Some(number) = normalized.strip_prefix("EVTEST_") {
        (number, false)
    } else {
        return Ok(None);
    };

    let parsed = if let Some(hex) = number.strip_prefix("0X") {
        u16::from_str_radix(hex, 16)
    } else {
        number.parse()
    }
    .map_err(|_| AppError::config(format!("invalid hotkey keycode: {value}")))?;
    let kernel_code = if subtract_xkb_offset {
        parsed
            .checked_sub(XKB_OFFSET)
            .ok_or_else(|| AppError::config(format!("XKB hotkey keycode too small: {value}")))?
    } else {
        parsed
    };
    Ok(Some(Key::new(kernel_code)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_right_control_hotkey_name() {
        assert_eq!(parse_key_name("RIGHTCTRL").unwrap(), Key::KEY_RIGHTCTRL);
        assert_eq!(parse_key_name("right-ctrl").unwrap(), Key::KEY_RIGHTCTRL);
        assert_eq!(parse_key_name("KEY_RIGHTCTRL").unwrap(), Key::KEY_RIGHTCTRL);
    }

    #[test]
    fn rejects_ambiguous_bare_numeric_keycodes() {
        assert!(parse_key_name("226").is_err());
    }

    #[test]
    fn converts_press_and_release_to_recording_events() {
        let mut pressed = false;
        assert_eq!(
            hotkey_event(Key::KEY_RIGHTCTRL, Key::KEY_RIGHTCTRL, 1, &mut pressed),
            Some(ControlEvent::StartRecording)
        );
        assert_eq!(
            hotkey_event(Key::KEY_RIGHTCTRL, Key::KEY_RIGHTCTRL, 2, &mut pressed),
            None
        );
        assert_eq!(
            hotkey_event(Key::KEY_RIGHTCTRL, Key::KEY_RIGHTCTRL, 0, &mut pressed),
            Some(ControlEvent::StopRecording)
        );
    }
}
