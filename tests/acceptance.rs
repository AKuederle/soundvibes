// Automated acceptance tests for docs/acceptance-tests.md.
// Keep AT-xx mappings in sync with the documentation.
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AttributeSet, EventType, InputEvent, Key};
#[cfg(feature = "test-support")]
use serde_json::Value;

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
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "test-support")]
use sv::daemon::test_support::{
    daemon_config, TestAudioBackend, TestOutput, TestTranscriberFactory,
};
use sv::daemon::ControlEvent;
#[cfg(feature = "test-support")]
use sv::daemon::{DaemonConfig, DaemonDeps, DaemonOutput};
#[cfg(feature = "test-support")]
use sv::error::AppError;
use sv::hotkey::{self, HotkeyConfig};
#[cfg(feature = "test-support")]
use sv::model::{ModelLanguage, ModelSize};
#[cfg(feature = "test-support")]
use sv::output::test_support::{RecordedCommand, TestRunner};
#[cfg(feature = "test-support")]
use sv::output::OutputConfig;
#[cfg(feature = "test-support")]
use sv::types::{OutputFormat, VadMode};

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
    let turbo_spec = sv::model::ModelSpec::new(
        sv::model::ModelSize::LargeV3Turbo,
        sv::model::ModelLanguage::Auto,
    );

    assert!(english_spec.filename_result()?.contains(".en."));
    assert!(!auto_spec.filename_result()?.contains(".en."));
    assert_eq!(turbo_spec.filename_result()?, "ggml-large-v3-turbo.bin");
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

#[test]
fn at04a_hotkey_keyboard_reconnects_without_listener_restart() -> Result<(), Box<dyn Error>> {
    if env::var("SV_HARDWARE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("Skipping AT-04a; set SV_HARDWARE_TESTS=1 to run.");
        return Ok(());
    }

    let Ok(mut keyboard) = virtual_keyboard() else {
        eprintln!("Skipping AT-04a; /dev/uinput is not writable.");
        return Ok(());
    };
    keyboard
        .enumerate_dev_nodes_blocking()?
        .next()
        .transpose()?;

    let (sender, receiver) = mpsc::channel();
    let config = HotkeyConfig {
        enabled: true,
        key: Some("RIGHTCTRL".to_string()),
    };
    let _listener = hotkey::start_listener(&config, sender)?;
    assert_virtual_hotkey(&mut keyboard, &receiver)?;

    drop(keyboard);
    let mut replacement = virtual_keyboard()?;
    replacement
        .enumerate_dev_nodes_blocking()?
        .next()
        .transpose()?;
    assert_virtual_hotkey(&mut replacement, &receiver)?;
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at04_daemon_hold_key_captures_and_transcribes() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
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
    let config = daemon_config();

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
    let (sender, receiver) = mpsc::channel();
    let control_sender = sender.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TestOutput::default();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
            "hello\n\"world\"\u{0008}".to_string(),
        ])),
    };
    let config = DaemonConfig {
        format: OutputFormat::Jsonl,
        ..daemon_config()
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
    assert_eq!(parsed["text"], "hello\n\"world\"\u{0008}");
    assert!(parsed["timestamp"].as_str().is_some());
    assert!(parsed["utterance"].as_u64().is_some());
    assert!(parsed["duration_ms"].as_u64().is_some());
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at05a_continuous_hold_key_transcribes_on_pause_before_release() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
    let control_sender = sender.clone();
    let (transcript_sender, transcript_receiver) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TranscriptSignalOutput {
        lines: Vec::new(),
        transcript_sender,
        transcript_marker: "Transcript 1: pause transcript",
    };
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
        vad: VadMode::Continuous,
        vad_silence_ms: 20,
        vad_chunk_ms: 10,
        segment_min_ms: 5,
        ..daemon_config()
    };

    let shutdown_trigger = Arc::clone(&shutdown);
    let transcript_before_release = Arc::new(AtomicBool::new(false));
    let transcript_before_release_trigger = Arc::clone(&transcript_before_release);
    let control_thread = thread::spawn(move || {
        let _ = control_sender.send(sv::daemon::ControlEvent::StartRecording);
        if transcript_receiver
            .recv_timeout(Duration::from_secs(1))
            .is_ok()
        {
            transcript_before_release_trigger.store(true, Ordering::Relaxed);
        }
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
    assert!(
        transcript_before_release.load(Ordering::Relaxed),
        "expected continuous mode to transcribe before key release"
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at05b_continuous_long_speech_transcribes_before_release() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
    let control_sender = sender.clone();
    let (transcript_sender, transcript_receiver) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TranscriptSignalOutput {
        lines: Vec::new(),
        transcript_sender,
        transcript_marker: "Transcript 1: timed transcript",
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 20], vec![0.2; 20], vec![0.2; 20]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
            "timed transcript".to_string()
        ])),
    };
    let config = DaemonConfig {
        sample_rate: 1_000,
        vad: VadMode::Continuous,
        vad_silence_ms: 100,
        vad_chunk_ms: 20,
        segment_target_ms: 20,
        segment_grace_ms: 20,
        segment_overlap_ms: 5,
        segment_min_ms: 10,
        ..daemon_config()
    };

    let shutdown_trigger = Arc::clone(&shutdown);
    let transcript_before_release = Arc::new(AtomicBool::new(false));
    let transcript_before_release_trigger = Arc::clone(&transcript_before_release);
    let control_thread = thread::spawn(move || {
        let _ = control_sender.send(sv::daemon::ControlEvent::StartRecording);
        if transcript_receiver
            .recv_timeout(Duration::from_secs(1))
            .is_ok()
        {
            transcript_before_release_trigger.store(true, Ordering::Relaxed);
        }
        let _ = control_sender.send(sv::daemon::ControlEvent::StopRecording);
        thread::sleep(Duration::from_millis(50));
        shutdown_trigger.store(true, Ordering::Relaxed);
    });

    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, shutdown.as_ref())?;
    control_thread.join().expect("control thread failed");

    assert!(output
        .stdout_lines()
        .iter()
        .any(|line| line.contains("Transcript 1: timed transcript")));
    assert!(
        transcript_before_release.load(Ordering::Relaxed),
        "expected long continuous speech to transcribe before key release"
    );
    Ok(())
}

#[cfg(feature = "test-support")]
struct TranscriptSignalOutput {
    lines: Vec<String>,
    transcript_sender: mpsc::Sender<()>,
    transcript_marker: &'static str,
}

#[cfg(feature = "test-support")]
impl TranscriptSignalOutput {
    fn stdout_lines(&self) -> &[String] {
        &self.lines
    }
}

#[cfg(feature = "test-support")]
impl DaemonOutput for TranscriptSignalOutput {
    fn stdout(&mut self, message: &str) {
        if message.contains(self.transcript_marker) {
            let _ = self.transcript_sender.send(());
        }
        self.lines.push(message.to_string());
    }

    fn stderr(&mut self, _message: &str) {}
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

#[cfg(feature = "test-support")]
#[test]
fn at11_paste_mode_restores_clipboard_with_original_mime() -> Result<(), Box<dyn Error>> {
    let _wayland_guard = EnvGuard::set("WAYLAND_DISPLAY", "wayland-0");
    let mut runner = TestRunner::default();
    runner.push_output(0, b"text/html\ntext/plain\n", b"");
    runner.push_output(0, b"<b>old</b>", b"");
    runner.push_status(0);
    runner.push_status(0);

    sv::output::output_text_with_runner("new text", &OutputConfig::default(), &mut runner)?;

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
        "dotool",
        &[],
        b"keydown leftctrl\nkey v\nkeyup leftctrl\n",
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

#[cfg(feature = "test-support")]
#[test]
fn at12_daemon_status_is_acknowledged() -> Result<(), Box<dyn Error>> {
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    fs::create_dir_all(&runtime_dir)?;
    let _runtime_guard = EnvGuard::set("XDG_RUNTIME_DIR", &runtime_dir);
    let socket_path = sv::daemon::daemon_socket_path()?;
    let (_socket_guard, receiver, _sender) = sv::daemon::start_socket_listener(&socket_path)?;

    let client = thread::spawn(|| -> Result<(), AppError> {
        let status = sv::daemon::send_status_command()?;
        assert!(status.ok);
        assert_eq!(status.state.as_deref(), Some("idle"));
        assert_eq!(status.language.as_deref(), Some("en"));

        let reload = sv::daemon::send_set_model_command(ModelSize::Tiny, ModelLanguage::En);
        let stopped = sv::daemon::send_stop_command()?;
        assert!(stopped.ok);
        let reload_error = reload.expect_err("missing model reload should fail");
        assert!(reload_error.to_string().contains("model file not found"));
        Ok(())
    });

    let config = daemon_config();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(vec!["Mic".to_string()], Vec::new())),
        transcriber_factory: Box::new(TestTranscriberFactory::new(Vec::new())),
    };
    let mut output = TestOutput::default();
    let shutdown = AtomicBool::new(false);
    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown)?;
    client.join().expect("control client failed")?;
    Ok(())
}

#[cfg(feature = "test-support")]
fn assert_command(command: &RecordedCommand, program: &str, args: &[&str], stdin: &[u8]) {
    let expected_args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    assert_eq!(command.program, program);
    assert_eq!(command.args, expected_args);
    assert_eq!(command.stdin, stdin);
}

fn virtual_keyboard() -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<Key>::new();
    for key in [Key::KEY_A, Key::KEY_Z, Key::KEY_ENTER, Key::KEY_RIGHTCTRL] {
        keys.insert(key);
    }
    VirtualDeviceBuilder::new()?
        .name("SoundVibes reconnect acceptance keyboard")
        .with_keys(&keys)?
        .build()
}

fn assert_virtual_hotkey(
    keyboard: &mut VirtualDevice,
    receiver: &mpsc::Receiver<ControlEvent>,
) -> Result<(), Box<dyn Error>> {
    let press = InputEvent::new(EventType::KEY, Key::KEY_RIGHTCTRL.code(), 1);
    let release = InputEvent::new(EventType::KEY, Key::KEY_RIGHTCTRL.code(), 0);
    let deadline = Instant::now() + Duration::from_secs(3);

    loop {
        keyboard.emit(&[press])?;
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(ControlEvent::StartRecording) => break,
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {
                keyboard.emit(&[release])?;
            }
            Ok(event) => return Err(format!("unexpected hotkey event: {event:?}").into()),
            Err(err) => return Err(format!("hotkey press was not received: {err}").into()),
        }
    }

    keyboard.emit(&[release])?;
    let event = receiver.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(event, ControlEvent::StopRecording));
    Ok(())
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
