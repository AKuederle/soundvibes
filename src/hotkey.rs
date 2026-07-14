use std::fs;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use evdev::{Device, InputEventKind, Key};
use serde::Deserialize;
use udev::{MonitorBuilder, MonitorSocket};

use crate::daemon::ControlEvent;
use crate::error::AppError;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeviceIdentity {
    device: u64,
    inode: u64,
}

impl From<&Metadata> for DeviceIdentity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug)]
struct DiscoveredDevice {
    path: PathBuf,
    identity: DeviceIdentity,
}

struct KeyboardDevice<T> {
    path: PathBuf,
    identity: DeviceIdentity,
    input: T,
    is_pressed: bool,
}

struct KeyboardDevices<T> {
    entries: Vec<KeyboardDevice<T>>,
}

impl<T> KeyboardDevices<T> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn reconcile(
        &mut self,
        discovered: Vec<DiscoveredDevice>,
        mut open: impl FnMut(&DiscoveredDevice) -> Option<T>,
    ) -> Option<ControlEvent> {
        let was_pressed = self.any_pressed();
        self.entries.retain(|current| {
            discovered.iter().any(|candidate| {
                candidate.path == current.path && candidate.identity == current.identity
            })
        });

        for candidate in discovered {
            let is_open = self.entries.iter().any(|current| {
                current.path == candidate.path && current.identity == candidate.identity
            });
            if !is_open {
                if let Some(input) = open(&candidate) {
                    self.entries.push(KeyboardDevice {
                        path: candidate.path,
                        identity: candidate.identity,
                        input,
                        is_pressed: false,
                    });
                }
            }
        }
        recording_transition(was_pressed, self.any_pressed())
    }

    fn remove(&mut self, index: usize) -> Option<ControlEvent> {
        let was_pressed = self.any_pressed();
        self.entries.remove(index);
        recording_transition(was_pressed, self.any_pressed())
    }

    fn set_pressed(&mut self, index: usize, is_pressed: bool) -> Option<ControlEvent> {
        let was_pressed = self.any_pressed();
        self.entries[index].is_pressed = is_pressed;
        recording_transition(was_pressed, self.any_pressed())
    }

    fn any_pressed(&self) -> bool {
        self.entries.iter().any(|device| device.is_pressed)
    }
}

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
    let monitor = input_device_monitor()?;
    let input_dir = Path::new("/dev/input");
    let mut devices = KeyboardDevices::new();
    reconcile_devices(&mut devices, input_dir)?;
    if devices.entries.is_empty() {
        return Err(AppError::runtime(
            "no keyboard device found in /dev/input; add your user to the input group or disable hotkey.enabled",
        ));
    }

    let handle = thread::spawn(move || {
        let mut last_reconcile = Instant::now();
        loop {
            let mut poll_fds = Vec::with_capacity(devices.entries.len() + 1);
            poll_fds.push(poll_fd(monitor.as_raw_fd()));
            poll_fds.extend(
                devices
                    .entries
                    .iter()
                    .map(|device| poll_fd(device.input.as_raw_fd())),
            );

            let timeout = reconcile_timeout(last_reconcile);
            let ready = unsafe {
                libc::poll(
                    poll_fds.as_mut_ptr(),
                    poll_fds.len() as libc::nfds_t,
                    timeout,
                )
            };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    eprintln!("hotkey listener poll error: {err}");
                }
                continue;
            }

            let monitor_changed = poll_fds[0].revents != 0;
            if monitor_changed {
                drain_monitor(&monitor);
            }

            let mut disconnected = Vec::new();
            for (index, poll_state) in poll_fds.iter().skip(1).enumerate() {
                if poll_state.revents == 0 {
                    continue;
                }
                if poll_state.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    disconnected.push(index);
                    continue;
                }

                let key_events = match devices.entries[index].input.fetch_events() {
                    Ok(events) => events
                        .filter_map(|event| match event.kind() {
                            InputEventKind::Key(key) => Some((key, event.value())),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Vec::new(),
                    Err(err) if err.raw_os_error() == Some(libc::ENODEV) => {
                        disconnected.push(index);
                        continue;
                    }
                    Err(err) => {
                        eprintln!("hotkey device read error: {err}");
                        continue;
                    }
                };

                for (key, value) in key_events {
                    let Some(is_pressed) = hotkey_press(target, key, value) else {
                        continue;
                    };
                    if !send_transition(&sender, devices.set_pressed(index, is_pressed)) {
                        return;
                    }
                }
            }

            disconnected.sort_unstable();
            disconnected.dedup();
            for index in disconnected.into_iter().rev() {
                if !send_transition(&sender, devices.remove(index)) {
                    return;
                }
            }

            if monitor_changed || last_reconcile.elapsed() >= RECONCILE_INTERVAL {
                match reconcile_devices(&mut devices, input_dir) {
                    Ok(event) => {
                        if !send_transition(&sender, event) {
                            return;
                        }
                        last_reconcile = Instant::now();
                    }
                    Err(err) => eprintln!("hotkey device reconciliation error: {err}"),
                }
            }
        }
    });
    Ok(handle)
}

fn input_device_monitor() -> Result<MonitorSocket, AppError> {
    MonitorBuilder::new()
        .and_then(|monitor| monitor.match_subsystem("input"))
        .and_then(MonitorBuilder::listen)
        .map_err(|err| AppError::runtime(format!("failed to monitor input devices: {err}")))
}

fn discover_devices(input_dir: &Path) -> Result<Vec<DiscoveredDevice>, AppError> {
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
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        devices.push(DiscoveredDevice {
            path,
            identity: DeviceIdentity::from(&metadata),
        });
    }
    devices.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(devices)
}

fn reconcile_devices(
    devices: &mut KeyboardDevices<Device>,
    input_dir: &Path,
) -> Result<Option<ControlEvent>, AppError> {
    let discovered = discover_devices(input_dir)?;
    Ok(devices.reconcile(discovered, |candidate| {
        let device = Device::open(&candidate.path).ok()?;
        if !is_keyboard(&device) {
            return None;
        }
        set_nonblocking(&device);
        Some(device)
    }))
}

fn poll_fd(fd: libc::c_int) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

fn reconcile_timeout(last_reconcile: Instant) -> libc::c_int {
    RECONCILE_INTERVAL
        .saturating_sub(last_reconcile.elapsed())
        .as_millis()
        .min(libc::c_int::MAX as u128) as libc::c_int
}

fn drain_monitor(monitor: &MonitorSocket) {
    while monitor.iter().next().is_some() {}
}

fn send_transition(sender: &Sender<ControlEvent>, event: Option<ControlEvent>) -> bool {
    event.is_none_or(|event| sender.send(event).is_ok())
}

fn is_event_device(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("event"))
}

fn is_keyboard(device: &Device) -> bool {
    device.supported_keys().is_some_and(|keys| {
        keys.contains(Key::KEY_A) && keys.contains(Key::KEY_Z) && keys.contains(Key::KEY_ENTER)
    })
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

fn hotkey_press(target: Key, key: Key, value: i32) -> Option<bool> {
    if key != target {
        return None;
    }
    match value {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    }
}

fn recording_transition(was_pressed: bool, is_pressed: bool) -> Option<ControlEvent> {
    match (was_pressed, is_pressed) {
        (false, true) => Some(ControlEvent::StartRecording),
        (true, false) => Some(ControlEvent::StopRecording),
        _ => None,
    }
}

pub fn parse_key_name(name: &str) -> Result<Key, AppError> {
    let trimmed = name.trim();
    if let Some(key) = parse_prefixed_keycode(trimmed)? {
        return Ok(key);
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

    if key_name.len() == 1 && key_name.chars().all(|c| c.is_ascii_digit()) {
        return Key::from_str(&format!("KEY_{key_name}"))
            .map_err(|_| AppError::config(format!("invalid hotkey key '{name}'")));
    }

    if trimmed.parse::<u16>().is_ok() || trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        return Err(AppError::config(format!(
            "bare numeric hotkey keycodes are ambiguous: {name}; use EVTEST_<code> or WEV_<code>"
        )));
    }

    if let Some(alias) = common_key_alias(key_name.as_str()) {
        return Ok(alias);
    }
    Key::from_str(&normalized)
        .or_else(|_| Key::from_str(&format!("KEY_{key_name}")))
        .map_err(|_| {
            AppError::config(format!(
                "unknown hotkey key '{name}'; use evtest to find a KEY_* name"
            ))
        })
}

fn common_key_alias(name: &str) -> Option<Key> {
    match name {
        "LEFT_CTRL" | "LCTRL" => Some(Key::KEY_LEFTCTRL),
        "RIGHT_CTRL" | "RCTRL" => Some(Key::KEY_RIGHTCTRL),
        "LALT" => Some(Key::KEY_LEFTALT),
        "RALT" => Some(Key::KEY_RIGHTALT),
        "ESC" => Some(Key::KEY_ESC),
        _ => None,
    }
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

    fn discovered(path: &str, inode: u64) -> DiscoveredDevice {
        DiscoveredDevice {
            path: PathBuf::from(path),
            identity: DeviceIdentity { device: 1, inode },
        }
    }

    #[test]
    fn reconciliation_adds_keyboards_discovered_after_startup() {
        let mut devices = KeyboardDevices::new();

        devices.reconcile(vec![discovered("event1", 1)], |device| {
            Some(device.path.clone())
        });
        devices.reconcile(
            vec![discovered("event1", 1), discovered("event2", 2)],
            |device| Some(device.path.clone()),
        );

        assert_eq!(
            devices
                .entries
                .iter()
                .map(|device| device.input.clone())
                .collect::<Vec<_>>(),
            [PathBuf::from("event1"), PathBuf::from("event2")]
        );
    }

    #[test]
    fn reconciliation_replaces_a_reused_event_node() {
        let mut devices = KeyboardDevices::new();
        devices.reconcile(vec![discovered("event1", 1)], |_| Some("original"));

        devices.reconcile(vec![discovered("event1", 2)], |_| Some("replacement"));

        assert_eq!(devices.entries.len(), 1);
        assert_eq!(devices.entries[0].input, "replacement");
    }

    #[test]
    fn disconnecting_the_last_pressed_keyboard_stops_recording() {
        let mut devices = KeyboardDevices::new();
        devices.reconcile(
            vec![discovered("event1", 1), discovered("event2", 2)],
            |_| Some(()),
        );
        assert_eq!(
            devices.set_pressed(0, true),
            Some(ControlEvent::StartRecording)
        );
        assert_eq!(devices.set_pressed(1, true), None);

        assert_eq!(
            devices.reconcile(vec![discovered("event2", 2)], |_| Some(())),
            None
        );
        assert_eq!(
            devices.reconcile(Vec::new(), |_| Some(())),
            Some(ControlEvent::StopRecording)
        );
    }

    #[test]
    fn parses_right_control_hotkey_name() {
        assert_eq!(parse_key_name("RIGHTCTRL").unwrap(), Key::KEY_RIGHTCTRL);
        assert_eq!(parse_key_name("right-ctrl").unwrap(), Key::KEY_RIGHTCTRL);
        assert_eq!(parse_key_name("KEY_RIGHTCTRL").unwrap(), Key::KEY_RIGHTCTRL);
    }

    #[test]
    fn parses_evtest_key_names() {
        assert_eq!(parse_key_name("F12").unwrap(), Key::KEY_F12);
        assert_eq!(parse_key_name("KEY_MENU").unwrap(), Key::KEY_MENU);
        assert_eq!(parse_key_name("A").unwrap(), Key::KEY_A);
        assert_eq!(parse_key_name("1").unwrap(), Key::KEY_1);
    }

    #[test]
    fn rejects_ambiguous_bare_numeric_keycodes() {
        assert!(parse_key_name("226").is_err());
    }

    #[test]
    fn converts_press_and_release_to_recording_events() {
        let mut devices = KeyboardDevices::new();
        devices.reconcile(vec![discovered("event1", 1)], |_| Some(()));
        assert_eq!(
            hotkey_press(Key::KEY_RIGHTCTRL, Key::KEY_RIGHTCTRL, 1),
            Some(true)
        );
        assert_eq!(
            devices.set_pressed(0, true),
            Some(ControlEvent::StartRecording)
        );
        assert_eq!(
            hotkey_press(Key::KEY_RIGHTCTRL, Key::KEY_RIGHTCTRL, 2),
            None
        );
        assert_eq!(
            hotkey_press(Key::KEY_RIGHTCTRL, Key::KEY_RIGHTCTRL, 0),
            Some(false)
        );
        assert_eq!(
            devices.set_pressed(0, false),
            Some(ControlEvent::StopRecording)
        );
    }
}
