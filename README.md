<div align="center">
  <img src="docs/assets/soundvibes.png" alt="SoundVibes Logo" width="200">
  <h1>SoundVibes (sv)</h1>
  <p>Open source voice-to-text for Linux</p>
</div>

## Overview

SoundVibes (sv) is an offline speech-to-text tool for Linux. It captures audio from your microphone while you hold a configured evdev key and transcribes locally using whisper.cpp. No cloud, no latency, no subscriptions.

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

### 3. Hold The Recording Key

Configure a Linux evdev key in `~/.config/soundvibes/config.toml`, then hold it while speaking and release it to finish the current recording.

```toml
[hotkey]
enabled = true
key = "RIGHTCTRL"
```

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

**Important:** Transcripts appear in the **daemon's output**.

```bash
# Terminal A: Start daemon (transcripts appear HERE)
sv daemon start --mode stdout

# Hold the configured hotkey to record.
# Release it to stop and transcribe.
```

**With systemd service:** View transcripts with:
```bash
journalctl --user -u sv.service -f
```

### Continuous Mode

Continuous mode transcribes and injects text after each pause in speech, even while the recording key is still held:

```bash
# Start daemon in continuous mode
sv daemon start --vad continuous

# Hold the configured hotkey and speak naturally.
# ... speak sentence 1 ... (pause) → text injected
# ... speak sentence 2 ... (pause) → text injected

# Release the key when done.
```

This mode uses whisper.cpp's Silero VAD model (~2MB, auto-downloaded on first use) to detect speech segments.

**Configuration:**
- `--vad continuous` - Enable continuous transcription mode
- `--vad-silence-ms` - Silence duration to trigger transcription (default: 800ms)

**Use cases:**
- Long-form dictation where you want text as you speak
- Hands-free note-taking with natural pauses

## Quick Tips

**Hotkey Setup:**
- SoundVibes reads `/dev/input/event*` via evdev, like VoxType.
- Your user must be able to read keyboard event devices, usually by being in the `input` group.
- Use `evtest` to find key names; configure the part after `KEY_`, for example `RIGHTCTRL`.

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
