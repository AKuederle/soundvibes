use std::env;
use std::fmt;
use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use clap::ValueEnum;
use serde::Deserialize;

const MAX_CLIPBOARD_BYTES: usize = 100 * 1024 * 1024;
const KDE_SECRET_MIME: &str = "x-kde-passwordManagerHint";

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    Stdout,
    Paste,
    Clipboard,
    Type,
    Ydotool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub mode: OutputMode,
    pub paste_keys: String,
    pub restore_clipboard: bool,
    pub pre_paste_delay_ms: u64,
    pub restore_clipboard_delay_ms: u64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Paste,
            paste_keys: "ctrl+v".to_string(),
            restore_clipboard: true,
            pre_paste_delay_ms: 100,
            restore_clipboard_delay_ms: 250,
        }
    }
}

#[derive(Debug)]
pub struct OutputError(String);

impl OutputError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OutputError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClipboardSnapshot {
    mime_type: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPasteKey {
    modifiers: Vec<KeyName>,
    key: KeyName,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum KeyName {
    Ctrl,
    Shift,
    Alt,
    Super,
    V,
    Insert,
    Enter,
}

pub trait CommandRunner {
    fn output(&mut self, program: &str, args: &[String]) -> Result<Output, std::io::Error>;
    fn status_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &[u8],
    ) -> Result<std::process::ExitStatus, std::io::Error>;
    fn copy_temporary_text(&mut self, text: &str) -> Result<(), OutputError>;
    fn sleep(&mut self, duration: Duration);
}

struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn output(&mut self, program: &str, args: &[String]) -> Result<Output, std::io::Error> {
        Command::new(program).args(args).output()
    }

    fn status_with_stdin(
        &mut self,
        program: &str,
        args: &[String],
        stdin: &[u8],
    ) -> Result<std::process::ExitStatus, std::io::Error> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(stdin)?;
        }
        child.wait()
    }

    fn copy_temporary_text(&mut self, text: &str) -> Result<(), OutputError> {
        use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

        let sources = vec![
            MimeSource {
                source: Source::Bytes(text.as_bytes().into()),
                mime_type: MimeType::Text,
            },
            MimeSource {
                source: Source::Bytes(b"secret"[..].into()),
                mime_type: MimeType::Specific(KDE_SECRET_MIME.to_string()),
            },
        ];

        Options::new()
            .copy_multi(sources)
            .map_err(|err| OutputError::new(format!("clipboard copy failed: {err}")))
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub fn output_text(text: &str, config: &OutputConfig) -> Result<(), OutputError> {
    let mut runner = SystemRunner;
    output_text_with_runner(text, config, &mut runner)
}

pub fn output_text_with_runner(
    text: &str,
    config: &OutputConfig,
    runner: &mut dyn CommandRunner,
) -> Result<(), OutputError> {
    if text.is_empty() {
        return Ok(());
    }

    match config.mode {
        OutputMode::Stdout => Ok(()),
        OutputMode::Paste => paste_text(text, config, runner),
        OutputMode::Clipboard => copy_plain_text(text, runner),
        OutputMode::Type => type_text(text, runner),
        OutputMode::Ydotool => type_text_ydotool(text, runner),
    }
}

fn paste_text(
    text: &str,
    config: &OutputConfig,
    runner: &mut dyn CommandRunner,
) -> Result<(), OutputError> {
    let paste_key = ParsedPasteKey::parse(&config.paste_keys)?;
    let original = if config.restore_clipboard {
        read_clipboard_snapshot(runner)?
    } else {
        None
    };
    let paste_result = (|| {
        runner.copy_temporary_text(text)?;
        runner.sleep(Duration::from_millis(config.pre_paste_delay_ms));
        send_paste_key_dotool(&paste_key, runner)
    })();

    if config.restore_clipboard {
        runner.sleep(Duration::from_millis(config.restore_clipboard_delay_ms));
        if let Err(err) = restore_clipboard_snapshot(original.as_ref(), runner) {
            eprintln!("warn: failed to restore clipboard: {err}");
        }
    }

    paste_result
}

fn read_clipboard_snapshot(
    runner: &mut dyn CommandRunner,
) -> Result<Option<ClipboardSnapshot>, OutputError> {
    if env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok(None);
    }

    let list_args = vec!["--list-types".to_string()];
    let types = runner.output("wl-paste", &list_args).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            OutputError::new("wl-paste not found; install wl-clipboard")
        } else {
            OutputError::new(format!("failed to run wl-paste: {err}"))
        }
    })?;
    if !types.status.success() {
        return Ok(None);
    }
    let mime_type = String::from_utf8_lossy(&types.stdout)
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if mime_type.is_empty() {
        return Ok(None);
    }

    let read_args = vec![
        "--type".to_string(),
        mime_type.clone(),
        "--no-newline".to_string(),
    ];
    let content = runner
        .output("wl-paste", &read_args)
        .map_err(|err| OutputError::new(format!("failed to read clipboard: {err}")))?;
    if !content.status.success() {
        return Ok(None);
    }
    if content.stdout.len() > MAX_CLIPBOARD_BYTES {
        return Err(OutputError::new(format!(
            "clipboard content too large to preserve: {} bytes",
            content.stdout.len()
        )));
    }

    Ok(Some(ClipboardSnapshot {
        mime_type,
        data: content.stdout,
    }))
}

fn restore_clipboard_snapshot(
    snapshot: Option<&ClipboardSnapshot>,
    runner: &mut dyn CommandRunner,
) -> Result<(), OutputError> {
    let (action, args, data) = match snapshot {
        Some(snapshot) => (
            "restore",
            vec!["--type".to_string(), snapshot.mime_type.clone()],
            snapshot.data.as_slice(),
        ),
        None => ("clear", vec!["--clear".to_string()], &[][..]),
    };
    let status = runner
        .status_with_stdin("wl-copy", &args, data)
        .map_err(|err| OutputError::new(format!("failed to {action} clipboard: {err}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(OutputError::new(format!(
            "wl-copy {action} exited with status {status}"
        )))
    }
}

fn copy_plain_text(text: &str, runner: &mut dyn CommandRunner) -> Result<(), OutputError> {
    let args = vec!["--type".to_string(), "text/plain".to_string()];
    let status = runner
        .status_with_stdin("wl-copy", &args, text.as_bytes())
        .map_err(|err| OutputError::new(format!("failed to copy text: {err}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(OutputError::new(format!(
            "wl-copy exited with status {status}"
        )))
    }
}

fn type_text(text: &str, runner: &mut dyn CommandRunner) -> Result<(), OutputError> {
    run_dotool(&dotool_type_script(text), "typing", runner)
}

fn type_text_ydotool(text: &str, runner: &mut dyn CommandRunner) -> Result<(), OutputError> {
    let args = vec![
        "type".to_string(),
        "--key-delay".to_string(),
        "0".to_string(),
        "--key-hold".to_string(),
        "0".to_string(),
        "--file".to_string(),
        "-".to_string(),
    ];
    let status = runner
        .status_with_stdin("ydotool", &args, text.as_bytes())
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                OutputError::new("ydotool not found; install ydotool and enable ydotoold")
            } else {
                OutputError::new(format!("failed to run ydotool: {err}"))
            }
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(OutputError::new(format!(
            "ydotool typing exited with status {status}; ensure ydotoold is running"
        )))
    }
}

fn run_dotool(
    script: &str,
    action: &str,
    runner: &mut dyn CommandRunner,
) -> Result<(), OutputError> {
    let status = runner
        .status_with_stdin("dotool", &[], script.as_bytes())
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                OutputError::new("dotool not found; install dotool")
            } else {
                OutputError::new(format!("failed to run dotool: {err}"))
            }
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(OutputError::new(format!(
            "dotool {action} exited with status {status}"
        )))
    }
}

fn dotool_type_script(text: &str) -> String {
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            '\n' => {
                if !current.is_empty() {
                    lines.push(format!("type {current}"));
                    current.clear();
                }
                lines.push("key enter".to_string());
            }
            '\r' => {}
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        lines.push(format!("type {current}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn send_paste_key_dotool(
    key: &ParsedPasteKey,
    runner: &mut dyn CommandRunner,
) -> Result<(), OutputError> {
    run_dotool(&key.to_dotool_script(), "paste key", runner)
}

impl ParsedPasteKey {
    fn parse(value: &str) -> Result<Self, OutputError> {
        let parts: Vec<&str> = value.split('+').map(str::trim).collect();
        if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
            return Err(OutputError::new("paste_keys has invalid format"));
        }

        let key = KeyName::parse(parts[parts.len() - 1])?;
        let modifiers = parts[..parts.len() - 1]
            .iter()
            .map(|part| KeyName::parse_modifier(part))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { modifiers, key })
    }

    fn to_dotool_script(&self) -> String {
        let mut lines = Vec::new();
        for modifier in &self.modifiers {
            lines.push(format!("keydown {}", modifier.dotool_name()));
        }
        lines.push(format!("key {}", self.key.dotool_name()));
        for modifier in self.modifiers.iter().rev() {
            lines.push(format!("keyup {}", modifier.dotool_name()));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

impl KeyName {
    fn parse(value: &str) -> Result<Self, OutputError> {
        match value.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "leftctrl" => Ok(Self::Ctrl),
            "shift" | "leftshift" => Ok(Self::Shift),
            "alt" | "leftalt" => Ok(Self::Alt),
            "super" | "meta" | "win" | "leftmeta" => Ok(Self::Super),
            "v" => Ok(Self::V),
            "insert" | "ins" => Ok(Self::Insert),
            "enter" | "return" => Ok(Self::Enter),
            other => Err(OutputError::new(format!("unknown paste key: {other}"))),
        }
    }

    fn parse_modifier(value: &str) -> Result<Self, OutputError> {
        let key = Self::parse(value)?;
        match key {
            Self::Ctrl | Self::Shift | Self::Alt | Self::Super => Ok(key),
            _ => Err(OutputError::new(format!(
                "paste key modifier expected, got {value}"
            ))),
        }
    }

    fn dotool_name(self) -> &'static str {
        match self {
            Self::Ctrl => "leftctrl",
            Self::Shift => "leftshift",
            Self::Alt => "leftalt",
            Self::Super => "leftmeta",
            Self::V => "v",
            Self::Insert => "insert",
            Self::Enter => "enter",
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RecordedCommand {
        pub program: String,
        pub args: Vec<String>,
        pub stdin: Vec<u8>,
    }

    #[derive(Default)]
    pub struct TestRunner {
        pub commands: Vec<RecordedCommand>,
        pub sleeps: Vec<Duration>,
        outputs: Vec<Output>,
        statuses: Vec<std::process::ExitStatus>,
    }

    impl TestRunner {
        pub fn push_output(&mut self, status: i32, stdout: &[u8], stderr: &[u8]) {
            self.outputs.push(Output {
                status: std::process::ExitStatus::from_raw(status << 8),
                stdout: stdout.to_vec(),
                stderr: stderr.to_vec(),
            });
        }

        pub fn push_status(&mut self, status: i32) {
            self.statuses
                .push(std::process::ExitStatus::from_raw(status << 8));
        }
    }

    impl CommandRunner for TestRunner {
        fn output(&mut self, program: &str, args: &[String]) -> Result<Output, std::io::Error> {
            self.commands.push(RecordedCommand {
                program: program.to_string(),
                args: args.to_vec(),
                stdin: Vec::new(),
            });
            Ok(self.outputs.remove(0))
        }

        fn status_with_stdin(
            &mut self,
            program: &str,
            args: &[String],
            stdin: &[u8],
        ) -> Result<std::process::ExitStatus, std::io::Error> {
            self.commands.push(RecordedCommand {
                program: program.to_string(),
                args: args.to_vec(),
                stdin: stdin.to_vec(),
            });
            Ok(self.statuses.remove(0))
        }

        fn copy_temporary_text(&mut self, text: &str) -> Result<(), OutputError> {
            self.commands.push(RecordedCommand {
                program: "temporary-clipboard-copy".to_string(),
                args: vec!["text/plain".to_string(), KDE_SECRET_MIME.to_string()],
                stdin: text.as_bytes().to_vec(),
            });
            Ok(())
        }

        fn sleep(&mut self, duration: Duration) {
            self.sleeps.push(duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TestRunner;
    use super::*;

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            env::set_var(key, value);
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
    fn paste_mode_clears_clipboard_when_it_started_empty() {
        let _guard = EnvGuard::set("WAYLAND_DISPLAY", "wayland-0");
        let mut runner = TestRunner::default();
        runner.push_output(1, b"", b"nothing is copied");
        runner.push_status(0);
        runner.push_output(0, b"", b"");
        runner.push_status(0);

        output_text_with_runner("new text", &OutputConfig::default(), &mut runner)
            .expect("paste should succeed");

        let clear = runner.commands.last().expect("clear command");
        assert_eq!(clear.program, "wl-copy");
        assert_eq!(clear.args, ["--clear"]);
        assert!(clear.stdin.is_empty());
    }

    #[test]
    fn paste_mode_rejects_invalid_paste_keys_before_changing_clipboard() {
        let mut runner = TestRunner::default();
        let config = OutputConfig {
            paste_keys: "ctrl+".to_string(),
            ..OutputConfig::default()
        };

        let err = output_text_with_runner("new text", &config, &mut runner)
            .expect_err("invalid paste key should fail");

        assert!(err.to_string().contains("paste_keys has invalid format"));
        assert!(
            runner.commands.is_empty(),
            "invalid config should not mutate clipboard"
        );
    }

    #[test]
    fn paste_mode_restores_clipboard_after_paste_key_failure() {
        let _guard = EnvGuard::set("WAYLAND_DISPLAY", "wayland-0");
        let mut runner = TestRunner::default();
        runner.push_output(0, b"text/plain\n", b"");
        runner.push_output(0, b"old", b"");
        runner.push_status(1);
        runner.push_status(0);

        let err = output_text_with_runner("new text", &OutputConfig::default(), &mut runner)
            .expect_err("paste key failure should be reported");

        assert!(err.to_string().contains("dotool paste key exited"));
        let restore = runner.commands.last().expect("restore command");
        assert_eq!(restore.program, "wl-copy");
        assert_eq!(restore.args, ["--type", "text/plain"]);
        assert_eq!(restore.stdin, b"old");
    }

    #[test]
    fn paste_mode_restores_original_clipboard_with_mime_type() {
        let _guard = EnvGuard::set("WAYLAND_DISPLAY", "wayland-0");
        let mut runner = TestRunner::default();
        runner.push_output(0, b"text/html\ntext/plain\n", b"");
        runner.push_output(0, b"<b>old</b>", b"");
        runner.push_status(0);
        runner.push_output(0, b"", b"");
        runner.push_status(0);

        output_text_with_runner("new text", &OutputConfig::default(), &mut runner)
            .expect("paste should succeed");

        assert_eq!(runner.commands[0].program, "wl-paste");
        assert_eq!(runner.commands[0].args, ["--list-types"]);
        assert_eq!(
            runner.commands[1].args,
            ["--type", "text/html", "--no-newline"]
        );
        assert_eq!(runner.commands[2].program, "temporary-clipboard-copy");
        assert_eq!(runner.commands[2].args, ["text/plain", KDE_SECRET_MIME]);
        assert_eq!(runner.commands[2].stdin, b"new text");
        assert_eq!(runner.commands[3].program, "dotool");
        assert!(runner.commands[3].args.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&runner.commands[3].stdin),
            "keydown leftctrl\nkey v\nkeyup leftctrl\n"
        );
        assert_eq!(runner.commands[4].program, "wl-copy");
        assert_eq!(runner.commands[4].args, ["--type", "text/html"]);
        assert_eq!(runner.commands[4].stdin, b"<b>old</b>");
        assert_eq!(
            runner.sleeps,
            [Duration::from_millis(100), Duration::from_millis(250)]
        );
    }

    #[test]
    fn type_mode_uses_dotool_without_wtype_fallback() {
        let mut runner = TestRunner::default();
        runner.push_status(0);
        let config = OutputConfig {
            mode: OutputMode::Type,
            ..OutputConfig::default()
        };

        output_text_with_runner("typed text", &config, &mut runner)
            .expect("type output should succeed");

        assert_eq!(runner.commands.len(), 1);
        assert_eq!(runner.commands[0].program, "dotool");
        assert!(runner.commands[0].args.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&runner.commands[0].stdin),
            "type typed text\n"
        );
    }

    #[test]
    fn ydotool_mode_uses_the_persistent_daemon_with_zero_delay() {
        let mut runner = TestRunner::default();
        runner.push_status(0);
        let config = OutputConfig {
            mode: OutputMode::Ydotool,
            ..OutputConfig::default()
        };

        output_text_with_runner("typed text", &config, &mut runner)
            .expect("ydotool output should succeed");

        assert_eq!(runner.commands.len(), 1);
        assert_eq!(runner.commands[0].program, "ydotool");
        assert_eq!(
            runner.commands[0].args,
            ["type", "--key-delay", "0", "--key-hold", "0", "--file", "-",]
        );
        assert_eq!(runner.commands[0].stdin, b"typed text");
    }

    #[test]
    fn ydotool_mode_reports_when_the_daemon_client_fails() {
        let mut runner = TestRunner::default();
        runner.push_status(1);
        let config = OutputConfig {
            mode: OutputMode::Ydotool,
            ..OutputConfig::default()
        };

        let err = output_text_with_runner("typed text", &config, &mut runner)
            .expect_err("ydotool failure should be reported");

        assert!(err.to_string().contains("ensure ydotoold is running"));
    }

    #[test]
    fn type_mode_splits_multiline_text_into_safe_dotool_commands() {
        let mut runner = TestRunner::default();
        runner.push_status(0);
        let config = OutputConfig {
            mode: OutputMode::Type,
            ..OutputConfig::default()
        };

        output_text_with_runner("first\nkey leftctrl+v\nthird", &config, &mut runner)
            .expect("type output should succeed");

        assert_eq!(
            String::from_utf8_lossy(&runner.commands[0].stdin),
            "type first\nkey enter\ntype key leftctrl+v\nkey enter\ntype third\n"
        );
    }
}
