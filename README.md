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

SoundVibes supports explicit output modes:

| Mode | Description | Use Case |
|------|-------------|----------|
| `paste` (default) | Copies text with `wl-copy`, pastes with `wtype`, then restores the previous clipboard | KDE/Wayland dictation |
| `clipboard` | Copies transcript to the clipboard for manual paste | Manual workflows |
| `type` | Types text directly with `wtype` | Wayland compositors that support virtual keyboard |
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

**Paste Output:**

Paste mode is the default. It uses `wl-copy`/`wl-paste` for clipboard capture and restore, and includes a KDE Klipper history-suppression MIME hint for the temporary transcription copy. The previous clipboard is restored after paste so dictated text does not remain in the active clipboard.

Configuration examples:

```bash
sv daemon start --mode paste --paste-keys ctrl+v
sv daemon start --mode paste --paste-keys ctrl+shift+v
```

## Requirements

- Linux x86_64
- Microphone input device
- Optional: Vulkan for GPU acceleration
- `wl-clipboard` (`wl-copy` and `wl-paste`) for paste/clipboard modes
- `wtype` for automatic paste key simulation and direct type mode

See the [website](https://soundvibes.teashaped.dev) for detailed requirements and configuration options.

## License

This project is licensed under the GNU General Public License v3.0 - see the [LICENSE](LICENSE) file for details.
