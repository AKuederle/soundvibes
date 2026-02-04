<div align="center">
  <img src="docs/assets/soundvibes.png" alt="SoundVibes Logo" width="200">
  <h1>SoundVibes (sv)</h1>
  <p>Open source voice-to-text for Linux</p>
</div>

## Overview

SoundVibes (sv) is an offline speech-to-text tool for Linux. It captures audio from your microphone using start/stop toggles and transcribes locally using whisper.cpp. No cloud, no latency, no subscriptions.

## Quick Start

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/kejne/soundvibes/main/install.sh | sh
```

Or download manually from [GitHub Releases](https://github.com/kejne/soundvibes/releases).

### 2. Start the Daemon

```bash
sv daemon start
```

### 3. Toggle Recording

```bash
sv
```

Bind the toggle command to a hotkey in your desktop environment for hands-free use.

## Documentation

- **Website**: [https://soundvibes.teashaped.dev](https://soundvibes.teashaped.dev) - Full documentation with installation guide, configuration reference, and troubleshooting
- **Contributing**: See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and build instructions

## Output Modes

SoundVibes supports two output modes:

| Mode | Description | Use Case |
|------|-------------|----------|
| `inject` (default) | Types text directly into focused window | Dictation into any app |
| `stdout` | Prints transcript to daemon's terminal | Scripting, debugging |

**Important:** Transcripts appear in the **daemon's output**, not the toggle command's output.

```bash
# Terminal A: Start daemon (transcripts appear HERE)
sv daemon start --mode stdout

# Terminal B: Toggle recording
sv          # start recording
sv          # stop and transcribe → output appears in Terminal A
```

**With systemd service:** View transcripts with:
```bash
journalctl --user -u sv.service -f
```

### Continuous Mode

Continuous mode transcribes and injects text after each pause in speech, without needing to toggle off:

```bash
# Start daemon in continuous mode
sv daemon start --vad continuous

# Toggle on, speak naturally with pauses, text appears after each pause
sv
# ... speak sentence 1 ... (pause) → text injected
# ... speak sentence 2 ... (pause) → text injected

# Toggle off when done
sv
```

This mode uses whisper.cpp's Silero VAD model (~2MB, auto-downloaded on first use) to detect speech segments.

**Configuration:**
- `--vad continuous` - Enable continuous transcription mode
- `--vad-silence-ms` - Silence duration to trigger transcription (default: 800ms)

**Use cases:**
- Long-form dictation where you want text as you speak
- Hands-free note-taking with natural pauses

## Quick Tips

**Desktop Environment Setup:**
- **i3/Sway**: `bindsym $mod+Shift+v exec sv`
- **Hyprland**: `bind = SUPER, V, exec, sv`
- **GNOME/KDE**: Add custom keyboard shortcut with command `sv`

**Systemd Service:**
```bash
systemctl --user enable --now sv.service
```

**Text Injection Backends:**

SoundVibes auto-detects the best backend, or you can force one with `--inject-backend`:

| Backend | Works On | Notes |
|---------|----------|-------|
| `ydotool` | All (Wayland + X11) | Recommended for KDE Plasma |
| `wtype` | Wayland (GNOME, Sway) | Doesn't work on KDE Plasma |
| `xdotool` | X11 only | |

**ydotool Setup (for KDE Plasma or universal use):**

```bash
# 1. Install ydotool
sudo pacman -S ydotool              # Arch
sudo apt install ydotool            # Debian/Ubuntu
sudo dnf install ydotool            # Fedora

# 2. Set up uinput permissions (required for non-root access)
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/80-uinput.rules
sudo udevadm control --reload-rules
sudo modprobe -r uinput && sudo modprobe uinput  # Reload module to apply permissions
sudo udevadm trigger

# 3. Add yourself to input group
sudo usermod -aG input $USER

# 4. Log out and log back in, then start ydotool
systemctl --user enable --now ydotool.service
```

## Requirements

- Linux x86_64
- Microphone input device
- Optional: Vulkan for GPU acceleration
- Optional: `ydotool` (universal), `wtype` (Wayland), or `xdotool` (X11) for text injection

See the [website](https://soundvibes.teashaped.dev) for detailed requirements and configuration options.

## License

This project is licensed under the GNU General Public License v3.0 - see the [LICENSE](LICENSE) file for details.
