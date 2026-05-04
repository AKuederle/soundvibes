// Automated acceptance tests for docs/acceptance-tests.md.
// Keep AT-xx mappings in sync with the documentation.
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(feature = "test-support")]
use std::os::unix::process::ExitStatusExt;
#[cfg(feature = "test-support")]
use std::process::{ExitStatus, Output};
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "test-support")]
use serde_json::Value;

#[cfg(feature = "test-support")]
use sv::daemon::test_support::{
    control_channel, TestAudioBackend, TestOutput, TestTranscriberFactory,
};
#[cfg(feature = "test-support")]
use sv::daemon::{DaemonConfig, DaemonDeps};
#[cfg(feature = "test-support")]
use sv::hotkey::HotkeyConfig;
#[cfg(feature = "test-support")]
use sv::output::{OutputConfig, OutputMode};
#[cfg(feature = "test-support")]
use sv::types::{AudioHost, OutputFormat, VadMode};

#[test]
fn at01_daemon_starts_with_valid_model() -> Result<(), Box<dyn Error>> {
    if env::var("SV_HARDWARE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("Skipping AT-01; set SV_HARDWARE_TESTS=1 to run.");
        return Ok(());
    }

    let model_path = model_path()?;
    if !model_path.exists() {
        eprintln!(
            "Skipping AT-01; model file not found at {}",
            model_path.display()
        );
        return Ok(());
    }

    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    write_config(
        &config_home,
        &format!("model = \"{}\"\n", model_path.display()),
    )?;

    let binary = env!("CARGO_BIN_EXE_sv");
    let mut child = Command::new(binary)
        .args(["daemon", "start"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout pipe");
    let (ready_tx, ready_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if line.contains("Daemon listening on") {
                let _ = ready_tx.send(line);
                break;
            }
        }
    });

    wait_for_daemon_ready(&mut child, ready_rx)?;

    stop_daemon(&mut child)?;
    let _ = reader_thread.join();
    Ok(())
}

#[test]
fn at01a_missing_model_is_auto_downloaded() -> Result<(), Box<dyn Error>> {
    let data_home = temp_dir("soundvibes-acceptance-data");
    let _data_guard = EnvGuard::set("XDG_DATA_HOME", &data_home);
    let payload = b"soundvibes-test-model".to_vec();
    let (base_url, server_handle) = start_test_server(payload.clone())?;
    let _url_guard = EnvGuard::set("SV_MODEL_BASE_URL", &base_url);

    let spec =
        sv::model::ModelSpec::new(sv::model::ModelSize::Auto, sv::model::ModelLanguage::Auto);
    let prepared = sv::model::prepare_model(None, &spec, true)?;

    assert!(prepared.downloaded, "expected model download");
    assert!(prepared.path.exists(), "expected model file to exist");
    let stored = fs::read(&prepared.path)?;
    assert_eq!(stored, payload, "downloaded model bytes mismatch");
    let _ = server_handle.join();
    Ok(())
}

#[test]
fn at01b_language_selects_model_variant() -> Result<(), Box<dyn Error>> {
    let english = sv::model::model_language_for_transcription("en");
    let auto = sv::model::model_language_for_transcription("auto");
    let other = sv::model::model_language_for_transcription("es");

    assert_eq!(english, sv::model::ModelLanguage::En);
    assert_eq!(auto, sv::model::ModelLanguage::Auto);
    assert_eq!(other, sv::model::ModelLanguage::Auto);

    let english_spec = sv::model::ModelSpec::new(sv::model::ModelSize::Small, english);
    let auto_spec = sv::model::ModelSpec::new(sv::model::ModelSize::Small, auto);

    assert!(english_spec.filename().contains(".en."));
    assert!(!auto_spec.filename().contains(".en."));
    Ok(())
}

#[test]
fn at02_missing_model_returns_exit_code_2() -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    let missing_path = temp_dir("soundvibes-missing-model").join("missing.bin");
    write_config(
        &config_home,
        &format!(
            "model = \"{}\"\ndownload_model = false\n",
            missing_path.display()
        ),
    )?;

    let binary = env!("CARGO_BIN_EXE_sv");
    let output = Command::new(binary)
        .args(["daemon", "start"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()?;

    let status = output.status.code().unwrap_or(-1);
    assert_eq!(status, 2, "expected exit code 2, got {status}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("model file not found"),
        "expected missing model error, got: {stderr}"
    );
    Ok(())
}

#[test]
fn at03_invalid_input_device_returns_exit_code_3() -> Result<(), Box<dyn Error>> {
    if env::var("SV_HARDWARE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("Skipping AT-03; set SV_HARDWARE_TESTS=1 to run.");
        return Ok(());
    }

    let model_path = model_path()?;
    if !model_path.exists() {
        eprintln!(
            "Skipping AT-03; model file not found at {}",
            model_path.display()
        );
        return Ok(());
    }

    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    write_config(
        &config_home,
        &format!(
            "model = \"{}\"\ndevice = \"nonexistent\"\n",
            model_path.display()
        ),
    )?;

    let binary = env!("CARGO_BIN_EXE_sv");
    let output = Command::new(binary)
        .args(["daemon", "start"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()?;

    let status = output.status.code().unwrap_or(-1);
    assert_eq!(status, 3, "expected exit code 3, got {status}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("input device not found"),
        "expected device error, got: {stderr}"
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at04_daemon_hold_key_captures_and_transcribes() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = control_channel();
    let control_sender = sender.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TestOutput::default();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec!["hello".to_string()])),
    };
    let config = DaemonConfig {
        model_path: None,
        download_model: false,
        language: "en".to_string(),
        device: None,
        audio_host: AudioHost::Default,
        sample_rate: 16_000,
        format: OutputFormat::Plain,
        mode: OutputMode::Stdout,
        output: OutputConfig::default(),
        vad: VadMode::Off,
        vad_silence_ms: 800,
        vad_threshold: 0.015,
        vad_chunk_ms: 250,
        debug_audio: false,
        debug_vad: false,
        dump_audio: false,
        vad_model_path: None,
        audio_feedback: false,
        no_speech_timeout_ms: 0,
        hotkey: HotkeyConfig::default(),
    };

    let shutdown_trigger = Arc::clone(&shutdown);
    let control_thread = thread::spawn(move || {
        let _ = control_sender.send(sv::daemon::ControlEvent::StartRecording);
        let _ = control_sender.send(sv::daemon::ControlEvent::StopRecording);
        thread::sleep(Duration::from_millis(50));
        shutdown_trigger.store(true, Ordering::Relaxed);
    });

    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, shutdown.as_ref())?;
    control_thread.join().expect("control thread failed");

    assert!(output
        .stdout_lines()
        .iter()
        .any(|line| line.contains("Transcript 1: hello")));
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at05_jsonl_output_formatting() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = control_channel();
    let control_sender = sender.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TestOutput::default();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec!["hello".to_string()])),
    };
    let config = DaemonConfig {
        model_path: None,
        download_model: false,
        language: "en".to_string(),
        device: None,
        audio_host: AudioHost::Default,
        sample_rate: 16_000,
        format: OutputFormat::Jsonl,
        mode: OutputMode::Stdout,
        output: OutputConfig::default(),
        vad: VadMode::Off,
        vad_silence_ms: 800,
        vad_threshold: 0.015,
        vad_chunk_ms: 250,
        debug_audio: false,
        debug_vad: false,
        dump_audio: false,
        vad_model_path: None,
        audio_feedback: false,
        no_speech_timeout_ms: 0,
        hotkey: HotkeyConfig::default(),
    };

    let shutdown_trigger = Arc::clone(&shutdown);
    let control_thread = thread::spawn(move || {
        let _ = control_sender.send(sv::daemon::ControlEvent::StartRecording);
        let _ = control_sender.send(sv::daemon::ControlEvent::StopRecording);
        thread::sleep(Duration::from_millis(50));
        shutdown_trigger.store(true, Ordering::Relaxed);
    });

    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, shutdown.as_ref())?;
    control_thread.join().expect("control thread failed");

    let json_line = output
        .stdout_lines()
        .iter()
        .find(|line| line.starts_with('{'))
        .ok_or("missing JSONL output")?;
    let parsed: Value = serde_json::from_str(json_line)?;
    assert_eq!(parsed["type"], "final");
    assert_eq!(parsed["text"], "hello");
    assert!(parsed["timestamp"].as_str().is_some());
    assert!(parsed["utterance"].as_u64().is_some());
    assert!(parsed["duration_ms"].as_u64().is_some());
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at05a_continuous_hold_key_transcribes_on_pause_before_release() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = control_channel();
    let control_sender = sender.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TestOutput::default();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160], vec![0.0; 400]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
            "pause transcript".to_string()
        ])),
    };
    let config = DaemonConfig {
        model_path: None,
        download_model: false,
        language: "en".to_string(),
        device: None,
        audio_host: AudioHost::Default,
        sample_rate: 16_000,
        format: OutputFormat::Plain,
        mode: OutputMode::Stdout,
        output: OutputConfig::default(),
        vad: VadMode::Continuous,
        vad_silence_ms: 20,
        vad_threshold: 0.015,
        vad_chunk_ms: 10,
        debug_audio: false,
        debug_vad: false,
        dump_audio: false,
        vad_model_path: None,
        audio_feedback: false,
        no_speech_timeout_ms: 0,
        hotkey: HotkeyConfig::default(),
    };

    let shutdown_trigger = Arc::clone(&shutdown);
    let control_thread = thread::spawn(move || {
        let _ = control_sender.send(sv::daemon::ControlEvent::StartRecording);
        thread::sleep(Duration::from_millis(120));
        let _ = control_sender.send(sv::daemon::ControlEvent::StopRecording);
        thread::sleep(Duration::from_millis(50));
        shutdown_trigger.store(true, Ordering::Relaxed);
    });

    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, shutdown.as_ref())?;
    control_thread.join().expect("control thread failed");

    assert!(output
        .stdout_lines()
        .iter()
        .any(|line| line.contains("Transcript 1: pause transcript")));
    Ok(())
}

#[test]
fn at06_offline_operation() -> Result<(), Box<dyn Error>> {
    if env::var("SV_HARDWARE_TESTS").ok().as_deref() != Some("1")
        || env::var("SV_OFFLINE_TESTS").ok().as_deref() != Some("1")
    {
        eprintln!("Skipping AT-06; set SV_HARDWARE_TESTS=1 and SV_OFFLINE_TESTS=1 to run.");
        return Ok(());
    }

    let model_path = model_path()?;
    if !model_path.exists() {
        eprintln!(
            "Skipping AT-06; model file not found at {}",
            model_path.display()
        );
        return Ok(());
    }

    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    write_config(
        &config_home,
        &format!("model = \"{}\"\n", model_path.display()),
    )?;

    let binary = env!("CARGO_BIN_EXE_sv");
    let mut child = Command::new(binary)
        .args(["daemon", "start"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout pipe");
    let (ready_tx, ready_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if line.contains("Daemon listening on") {
                let _ = ready_tx.send(line);
                break;
            }
        }
    });

    wait_for_daemon_ready(&mut child, ready_rx)?;
    stop_daemon(&mut child)?;
    let _ = reader_thread.join();
    Ok(())
}

#[test]
fn at07_gpu_auto_select() -> Result<(), Box<dyn Error>> {
    if env::var("SV_HARDWARE_TESTS").ok().as_deref() != Some("1")
        || env::var("SV_GPU_TESTS").ok().as_deref() != Some("1")
    {
        eprintln!("Skipping AT-07 GPU check; set SV_HARDWARE_TESTS=1 and SV_GPU_TESTS=1 to run.");
        return Ok(());
    }

    let model_path = model_path()?;
    if !model_path.exists() {
        eprintln!(
            "Skipping AT-07 GPU check; model file not found at {}",
            model_path.display()
        );
        return Ok(());
    }

    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    write_config(
        &config_home,
        &format!("model = \"{}\"\n", model_path.display()),
    )?;

    let stderr_lines = run_daemon_for_logs(&config_home, &runtime_dir)?;
    let stderr_joined = stderr_lines.join("\n");
    assert!(
        stderr_joined.contains("whisper: GPU backend selected"),
        "expected GPU backend selection, got: {stderr_joined}"
    );
    Ok(())
}

#[test]
fn at07_cpu_fallback() -> Result<(), Box<dyn Error>> {
    if env::var("SV_HARDWARE_TESTS").ok().as_deref() != Some("1")
        || env::var("SV_CPU_TESTS").ok().as_deref() != Some("1")
    {
        eprintln!("Skipping AT-07 CPU check; set SV_HARDWARE_TESTS=1 and SV_CPU_TESTS=1 to run.");
        return Ok(());
    }

    let model_path = model_path()?;
    if !model_path.exists() {
        eprintln!(
            "Skipping AT-07 CPU check; model file not found at {}",
            model_path.display()
        );
        return Ok(());
    }

    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    write_config(
        &config_home,
        &format!("model = \"{}\"\n", model_path.display()),
    )?;

    let stderr_lines = run_daemon_for_logs(&config_home, &runtime_dir)?;
    let stderr_joined = stderr_lines.join("\n");
    assert!(
        stderr_joined.contains("using CPU"),
        "expected CPU fallback message, got: {stderr_joined}"
    );
    Ok(())
}

#[test]
fn at10_marketing_site_builds_and_smoke_test() -> Result<(), Box<dyn Error>> {
    if env::var("SV_WEB_TESTS").ok().as_deref() != Some("1") {
        eprintln!("Skipping AT-10; set SV_WEB_TESTS=1 to run.");
        return Ok(());
    }

    if !command_available("npm") {
        eprintln!("Skipping AT-10; npm not available in PATH.");
        return Ok(());
    }

    let web_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    if !web_root.exists() {
        eprintln!("Skipping AT-10; web/ directory missing.");
        return Ok(());
    }

    let install_status = Command::new("npm")
        .arg("install")
        .arg("--no-audit")
        .arg("--no-fund")
        .current_dir(&web_root)
        .status()?;
    if !install_status.success() {
        return Err("AT-10 failed: npm install did not exit cleanly".into());
    }

    let build_status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&web_root)
        .status()?;
    if !build_status.success() {
        return Err("AT-10 failed: npm run build did not exit cleanly".into());
    }

    let smoke_status = Command::new("npm")
        .args(["run", "test:ui"])
        .current_dir(&web_root)
        .status()?;
    if !smoke_status.success() {
        return Err("AT-10 failed: npm run test:ui did not exit cleanly".into());
    }

    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at11_paste_mode_restores_clipboard_with_original_mime() -> Result<(), Box<dyn Error>> {
    let _wayland_guard = EnvGuard::set("WAYLAND_DISPLAY", "wayland-0");
    let mut runner = AcceptanceRunner::default();
    runner.push_output(0, b"text/html\ntext/plain\n");
    runner.push_output(0, b"<b>old</b>");
    runner.push_output(0, b"");
    runner.push_status(0);

    sv::output::output_text_with_runner(
        "new text",
        OutputMode::Paste,
        &OutputConfig::default(),
        &mut runner,
    )?;

    assert_eq!(runner.commands.len(), 5);
    assert_command(&runner.commands[0], "wl-paste", &["--list-types"], b"");
    assert_command(
        &runner.commands[1],
        "wl-paste",
        &["--type", "text/html", "--no-newline"],
        b"",
    );
    assert_command(
        &runner.commands[2],
        "temporary-clipboard-copy",
        &["text/plain", "x-kde-passwordManagerHint"],
        b"new text",
    );
    assert_command(
        &runner.commands[3],
        "wtype",
        &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
        b"",
    );
    assert_command(
        &runner.commands[4],
        "wl-copy",
        &["--type", "text/html"],
        b"<b>old</b>",
    );
    assert_eq!(
        runner.sleeps,
        vec![Duration::from_millis(100), Duration::from_millis(250)]
    );
    Ok(())
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AcceptanceCommand {
    program: String,
    args: Vec<String>,
    stdin: Vec<u8>,
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct AcceptanceRunner {
    commands: Vec<AcceptanceCommand>,
    outputs: Vec<Output>,
    statuses: Vec<ExitStatus>,
    sleeps: Vec<Duration>,
}

#[cfg(feature = "test-support")]
impl AcceptanceRunner {
    fn push_output(&mut self, status: i32, stdout: &[u8]) {
        self.outputs.push(Output {
            status: ExitStatus::from_raw(status),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        });
    }

    fn push_status(&mut self, status: i32) {
        self.statuses.push(ExitStatus::from_raw(status));
    }
}

#[cfg(feature = "test-support")]
impl sv::output::CommandRunner for AcceptanceRunner {
    fn output(&mut self, program: &str, args: &[String]) -> Result<Output, std::io::Error> {
        self.commands.push(AcceptanceCommand {
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
    ) -> Result<ExitStatus, std::io::Error> {
        self.commands.push(AcceptanceCommand {
            program: program.to_string(),
            args: args.to_vec(),
            stdin: stdin.to_vec(),
        });
        Ok(self.statuses.remove(0))
    }

    fn copy_temporary_text(&mut self, text: &str) -> Result<(), sv::output::OutputError> {
        self.commands.push(AcceptanceCommand {
            program: "temporary-clipboard-copy".to_string(),
            args: vec![
                "text/plain".to_string(),
                "x-kde-passwordManagerHint".to_string(),
            ],
            stdin: text.as_bytes().to_vec(),
        });
        Ok(())
    }

    fn sleep(&mut self, duration: Duration) {
        self.sleeps.push(duration);
    }
}

#[cfg(feature = "test-support")]
fn assert_command(command: &AcceptanceCommand, program: &str, args: &[&str], stdin: &[u8]) {
    let expected_args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    assert_eq!(command.program, program);
    assert_eq!(command.args, expected_args);
    assert_eq!(command.stdin, stdin);
}

fn model_path() -> Result<PathBuf, Box<dyn Error>> {
    if let Ok(path) = env::var("SV_MODEL_PATH") {
        return Ok(PathBuf::from(path));
    }
    let data_home = env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    Ok(data_home
        .join("soundvibes")
        .join("models")
        .join("ggml-base.en.bin"))
}

fn temp_dir(prefix: &str) -> PathBuf {
    let mut dir = env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    dir.push(format!("{prefix}-{}-{stamp}", std::process::id()));
    dir
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
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

fn start_test_server(payload: Vec<u8>) -> Result<(String, thread::JoinHandle<()>), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&payload);
        }
    });
    Ok((format!("http://{addr}"), handle))
}

fn write_config(config_home: &std::path::Path, contents: &str) -> Result<(), Box<dyn Error>> {
    let config_path = config_home.join("soundvibes").join("config.toml");
    fs::create_dir_all(config_path.parent().expect("config parent"))?;
    fs::write(&config_path, contents)?;
    Ok(())
}

fn wait_for_daemon_ready(
    child: &mut std::process::Child,
    ready_rx: mpsc::Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();
    loop {
        if let Ok(line) = ready_rx.recv_timeout(Duration::from_millis(100)) {
            if line.contains("Daemon listening on") {
                return Ok(());
            }
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("daemon exited early with {status}").into());
        }
        if start.elapsed() > Duration::from_secs(3) {
            return Err("daemon did not report ready state".into());
        }
    }
}

fn stop_daemon(child: &mut std::process::Child) -> Result<(), Box<dyn Error>> {
    let pid = child.id();
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()?;
    if !status.success() {
        return Err("failed to send SIGTERM to daemon".into());
    }

    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!("daemon exited with {status}").into());
            }
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(3) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
    let _ = child.wait();
    Err("daemon did not terminate after SIGTERM".into())
}

fn run_daemon_for_logs(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
) -> Result<Vec<String>, Box<dyn Error>> {
    let binary = env!("CARGO_BIN_EXE_sv");
    let mut child = Command::new(binary)
        .args(["daemon", "start"])
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let (ready_tx, ready_rx) = mpsc::channel();
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_capture = Arc::clone(&stderr_lines);

    let stdout_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if line.contains("Daemon listening on") {
                let _ = ready_tx.send(line);
                break;
            }
        }
    });

    let stderr_thread = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            stderr_capture.lock().expect("stderr lock").push(line);
        }
    });

    wait_for_daemon_ready(&mut child, ready_rx)?;
    stop_daemon(&mut child)?;
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    let collected = stderr_lines.lock().expect("stderr lock").clone();
    Ok(collected)
}
