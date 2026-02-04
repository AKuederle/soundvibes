# ydotool Support and Systemd Service Fix

## Overview

Two improvements to make SoundVibes work reliably on KDE Plasma (Wayland) and with systemd user services.

## Feature 1: ydotool Backend Support

### Problem

The current text injection backends (`wtype` for Wayland, `xdotool` for X11) don't work on KDE Plasma's Wayland compositor because KDE doesn't implement the `zwp_virtual_keyboard_v1` protocol that `wtype` requires.

### Solution

Add `ydotool` as a text injection backend. Unlike `wtype`, `ydotool` works at the kernel level via uinput, bypassing compositor protocol limitations. It works on both Wayland and X11.

### Implementation

**File: `src/output.rs`**

1. Add `try_ydotool()` function that:
   - Checks if ydotoold is running (socket exists at `/run/user/$UID/.ydotool_socket` or `/tmp/.ydotool_socket`)
   - Runs `ydotool type -- <text>`

2. Update `inject_text()` priority order:
   1. `ydotool` - works everywhere if daemon running
   2. `wtype` - Wayland compositor protocol
   3. `xdotool` - X11 only

3. Add CLI flag `--inject-backend <auto|ydotool|wtype|xdotool>` to force a specific backend (default: `auto`).

**File: `src/main.rs`**

4. Add `--inject-backend` to CLI args and pass to daemon.

### Detection Logic

```rust
fn has_ydotool() -> bool {
    let uid = unsafe { libc::getuid() };
    let socket_paths = [
        format!("/run/user/{}/.ydotool_socket", uid),
        "/tmp/.ydotool_socket".to_string(),
    ];
    socket_paths.iter().any(|p| Path::new(p).exists())
}
```

### Command

```
ydotool type -- "transcribed text"
```

## Feature 2: Systemd Service Fix

### Problem

1. No service file exists in the repo despite README documenting `systemctl --user enable --now sv.service`
2. When users create their own service file, display session variables (`DISPLAY`, `WAYLAND_DISPLAY`) aren't available because the service starts before the graphical session

### Solution

Add a proper systemd user service file that starts after the graphical session.

### Implementation

**File: `contrib/sv.service`**

```ini
[Unit]
Description=SoundVibes speech-to-text daemon
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/sv daemon start
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
```

Key points:
- `After=graphical-session.target` - waits for display session
- `WantedBy=graphical-session.target` - only starts when graphical session is active (fixes #40)
- `%h` - expands to user's home directory
- `RestartSec=5` - prevents rapid restart loops

**File: `install.sh`**

Update install script to:
1. Copy service file to `~/.config/systemd/user/sv.service`
2. Run `systemctl --user daemon-reload`
3. Print instructions for enabling the service

## Testing

### ydotool
1. Install ydotool: `sudo pacman -S ydotool`
2. Start daemon: `sudo systemctl enable --now ydotool`
3. Run `sv daemon start` then `sv` to test injection

### Systemd
1. Install service: copy to `~/.config/systemd/user/`
2. Enable: `systemctl --user enable sv.service`
3. Reboot and verify service starts and can inject text

## References

- Issue #40: https://github.com/kejne/soundvibes/issues/40
- ydotool: https://github.com/ReimuNotMoe/ydotool
