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
use sv::daemon::{Activity, ControlEvent};
#[cfg(feature = "test-support")]
use sv::daemon::{DaemonConfig, DaemonDeps, DaemonOutput};
#[cfg(feature = "test-support")]
use sv::error::AppError;
use sv::hotkey::{self, HotkeyConfig};
#[cfg(feature = "test-support")]
use sv::output::test_support::{RecordedCommand, TestRunner};
#[cfg(feature = "test-support")]
use sv::output::OutputConfig;
#[cfg(feature = "test-support")]
use sv::types::{OutputFormat, VadMode};

#[cfg(feature = "test-support")]
#[derive(Default)]
struct VocabularyTranscriber {
    words: Vec<String>,
}

#[cfg(feature = "test-support")]
impl sv::daemon::Transcriber for VocabularyTranscriber {
    fn set_vocabulary(&mut self, words: &[String]) -> Result<Option<String>, AppError> {
        self.words = words.to_vec();
        Ok(None)
    }

    fn transcribe(&self, _samples: &[f32], _language: Option<&str>) -> Result<String, AppError> {
        Ok(if self.words.is_empty() {
            "<empty>".to_string()
        } else {
            self.words.join(" | ")
        })
    }
}

#[cfg(feature = "test-support")]
struct VocabularyFactory;

#[cfg(feature = "test-support")]
impl sv::daemon::TranscriberFactory for VocabularyFactory {
    fn load(
        &self,
        _path: Option<&std::path::Path>,
    ) -> Result<Box<dyn sv::daemon::Transcriber>, AppError> {
        Ok(Box::new(VocabularyTranscriber::default()))
    }
}

#[cfg(feature = "test-support")]
struct LiveOnlyFactory(&'static str);
#[cfg(feature = "test-support")]
impl sv::daemon::TranscriberFactory for LiveOnlyFactory {
    fn load(
        &self,
        _: Option<&std::path::Path>,
    ) -> Result<Box<dyn sv::daemon::Transcriber>, AppError> {
        Ok(Box::new(LiveOnlyTranscriber(self.0)))
    }
}
#[cfg(feature = "test-support")]
struct LiveOnlyTranscriber(&'static str);
#[cfg(feature = "test-support")]
impl sv::daemon::Transcriber for LiveOnlyTranscriber {
    fn set_vocabulary(&mut self, _: &[String]) -> Result<Option<String>, AppError> {
        Ok(None)
    }
    fn transcribe(&self, _: &[f32], _: Option<&str>) -> Result<String, AppError> {
        Err(AppError::runtime("unexpected offline fallback"))
    }
    fn begin_stream(
        &mut self,
        _: Option<&str>,
    ) -> Result<Option<Box<dyn sv::daemon::StreamingTranscriber + '_>>, AppError> {
        Ok(Some(Box::new(LiveOnlySession {
            text: self.0,
            received: 0,
        })))
    }
}
#[cfg(feature = "test-support")]
struct LiveOnlySession {
    text: &'static str,
    received: usize,
}
#[cfg(feature = "test-support")]
impl sv::daemon::StreamingTranscriber for LiveOnlySession {
    fn feed(&mut self, samples: &[f32]) -> Result<(), AppError> {
        self.received += samples.len();
        Ok(())
    }
    fn finish(&mut self) -> Result<String, AppError> {
        Ok(if self.received == 0 {
            String::new()
        } else {
            self.text.into()
        })
    }
}

#[cfg(feature = "test-support")]
fn acknowledge(sender: &mpsc::Sender<ControlEvent>, event: ControlEvent) -> Result<(), String> {
    acknowledge_with_timeout(sender, event, Duration::from_secs(5))
}

#[cfg(feature = "test-support")]
fn acknowledge_with_timeout(
    sender: &mpsc::Sender<ControlEvent>,
    event: ControlEvent,
    timeout: Duration,
) -> Result<(), String> {
    let (reply, response) = mpsc::sync_channel(1);
    sender
        .send(ControlEvent::Request {
            event: Box::new(event),
            respond_to: reply,
        })
        .map_err(|e| e.to_string())?;
    let result = response.recv_timeout(timeout).map_err(|e| e.to_string())?;
    if result.ok {
        Ok(())
    } else {
        Err(result.message.unwrap_or_default())
    }
}

#[cfg(feature = "test-support")]
#[test]
fn at17_vocabulary_files_reload_and_merge_without_restarting_daemon() -> Result<(), Box<dyn Error>>
{
    let directory = temp_dir("soundvibes-vocabulary");
    let config = DaemonConfig {
        vocabulary_dir: Some(directory.clone()),
        ..daemon_config()
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160]; 40],
        )),
        transcriber_factory: Box::new(VocabularyFactory),
    };
    let (sender, receiver) = mpsc::channel();
    let controller = thread::spawn(move || -> Result<(), String> {
        let utterance = || {
            acknowledge(&sender, ControlEvent::StartRecording)?;
            acknowledge(&sender, ControlEvent::StopRecording)
        };
        utterance()?; // Missing at startup.
        fs::create_dir_all(directory.join("nested.txt")).map_err(|e| e.to_string())?;
        fs::write(directory.join("b.txt"), "PostgreSQL\nada lovelace\n")
            .map_err(|e| e.to_string())?;
        fs::write(
            directory.join("a.txt"),
            " # names\n\n  SoundVibes \n Ada   Lovelace\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(directory.join(".hidden.txt"), "hidden").map_err(|e| e.to_string())?;
        fs::write(directory.join("b.txt.tmp"), "unfinished").map_err(|e| e.to_string())?;
        fs::write(directory.join("nested.txt/child.txt"), "nested").map_err(|e| e.to_string())?;
        utterance()?;
        fs::write(directory.join("a.txt"), "soundvibes\nNeutrino\nC#\n")
            .map_err(|e| e.to_string())?;
        utterance()?;
        fs::rename(directory.join("b.txt.tmp"), directory.join("b.txt"))
            .map_err(|e| e.to_string())?;
        utterance()?;
        fs::remove_file(directory.join("a.txt")).map_err(|e| e.to_string())?;
        utterance()?;
        fs::remove_dir_all(&directory).map_err(|e| e.to_string())?;
        utterance()?;
        acknowledge(&sender, ControlEvent::Stop)
    });
    let mut output = TestOutput::default();
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    controller.join().map_err(|_| "controller panicked")??;
    let transcripts: Vec<&str> = output
        .stdout_lines()
        .iter()
        .filter(|line| line.starts_with("Transcript "))
        .map(|line| line.split_once(": ").unwrap().1)
        .collect();
    assert_eq!(
        transcripts,
        [
            "<empty>",
            "SoundVibes | Ada Lovelace | PostgreSQL",
            "soundvibes | Neutrino | C# | PostgreSQL | ada lovelace",
            "soundvibes | Neutrino | C# | unfinished",
            "unfinished",
            "<empty>",
        ]
    );
    assert!(
        output.stderr_lines().is_empty(),
        "{:?}",
        output.stderr_lines()
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at17a_bad_vocabulary_updates_keep_last_good_list_and_recover() -> Result<(), Box<dyn Error>> {
    let directory = temp_dir("soundvibes-vocabulary-errors");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("app.txt"), "Quillon\n")?;
    let config = DaemonConfig {
        vocabulary_dir: Some(directory.clone()),
        ..daemon_config()
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160]; 40],
        )),
        transcriber_factory: Box::new(VocabularyFactory),
    };
    let (sender, receiver) = mpsc::channel();
    let controller = thread::spawn(move || -> Result<(), String> {
        let utterance = || {
            acknowledge(&sender, ControlEvent::StartRecording)?;
            acknowledge(&sender, ControlEvent::StopRecording)
        };
        utterance()?;
        fs::write(directory.join("app.txt"), "Neutrino\n").map_err(|e| e.to_string())?;
        fs::write(directory.join("z.txt"), [0xff]).map_err(|e| e.to_string())?;
        utterance()?;
        fs::write(directory.join("z.txt"), "bad\0term\n").map_err(|e| e.to_string())?;
        utterance()?;
        fs::remove_file(directory.join("z.txt")).map_err(|e| e.to_string())?;
        utterance()?;
        fs::remove_dir_all(&directory).map_err(|e| e.to_string())?;
        fs::write(&directory, "not a directory").map_err(|e| e.to_string())?;
        utterance()?;
        fs::remove_file(&directory).map_err(|e| e.to_string())?;
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        fs::write(directory.join("app.txt"), "Ada\n").map_err(|e| e.to_string())?;
        utterance()?;
        fs::remove_dir_all(directory).map_err(|e| e.to_string())?;
        acknowledge(&sender, ControlEvent::Stop)
    });
    let mut output = TestOutput::default();
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    controller.join().map_err(|_| "controller panicked")??;
    let transcripts: Vec<&str> = output
        .stdout_lines()
        .iter()
        .filter(|line| line.starts_with("Transcript "))
        .map(|line| line.split_once(": ").unwrap().1)
        .collect();
    assert_eq!(
        transcripts,
        ["Quillon", "Quillon", "Quillon", "Neutrino", "Neutrino", "Ada"]
    );
    assert_eq!(
        output.stderr_lines().len(),
        3,
        "{:?}",
        output.stderr_lines()
    );
    assert!(output
        .stderr_lines()
        .iter()
        .all(|line| line.contains("keeping the previous vocabulary")));
    Ok(())
}

#[test]
fn at16_phonon2_rejects_unsupported_audio_configuration() -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-phonon2-config");
    write_config(&config_home, "")?;
    for (args, message) in [
        (vec!["--language", "es"], "English only"),
        (vec!["--sample-rate", "48000"], "16000 Hz"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sv"))
            .args(["daemon", "start", "--engine", "phonon2"])
            .args(args)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
    }
    fs::remove_dir_all(config_home)?;
    Ok(())
}

#[test]
fn at16a_phonon2_missing_assets_report_setup() -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-phonon2-config");
    let model_dir = temp_dir("soundvibes-phonon2-model");
    fs::create_dir_all(&model_dir)?;
    write_config(&config_home, "engine = \"phonon2\"\n")?;
    let output = Command::new(env!("CARGO_BIN_EXE_sv"))
        .args(["daemon", "start", "--model"])
        .arg(&model_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Phonon-2 model not found") && stderr.contains("prepare-phonon2.py"),
        "{stderr}"
    );
    fs::remove_dir_all(config_home)?;
    fs::remove_dir_all(model_dir)?;
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at16b_phonon2_real_model_transcribes_through_daemon_worker() -> Result<(), Box<dyn Error>> {
    let Some(model_dir) = env::var_os("PHONON2_MODEL_DIR") else {
        eprintln!("Skipping AT-16b; set PHONON2_MODEL_DIR and PHONON2_LIBRARY_DIR.");
        return Ok(());
    };
    let library_dir = env::var_os("PHONON2_LIBRARY_DIR").ok_or("PHONON2_LIBRARY_DIR required")?;
    let vocabulary_dir = temp_dir("soundvibes-phonon2-vocabulary");
    fs::create_dir_all(&vocabulary_dir)?;
    let terms = (0..26)
        .map(|i| format!("ProjectTerm{i}\n"))
        .collect::<String>();
    fs::write(vocabulary_dir.join("app.txt"), terms)?;
    let mut reader = hound::WavReader::open("tests/fixtures/jfk.wav")?;
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    let config = DaemonConfig {
        engine: sv::inference::InferenceEngine::Phonon2,
        phonon2_library_dir: Some(PathBuf::from(library_dir)),
        vocabulary_dir: Some(vocabulary_dir.clone()),
        model_path: Some(PathBuf::from(model_dir)),
        vad: VadMode::Off,
        ..daemon_config()
    };
    let mut deps = DaemonDeps::for_config(&config)?;
    deps.audio = Box::new(TestAudioBackend::new(vec!["Fixture".into()], vec![samples]));
    let (sender, receiver) = mpsc::channel();
    sender.send(ControlEvent::StartRecording)?;
    sender.send(ControlEvent::StopRecording)?;
    sender.send(ControlEvent::Stop)?;
    let mut output = TestOutput::default();
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    assert!(
        output
            .stderr_lines()
            .iter()
            .any(|line| line.contains("first 25 of 26 terms")),
        "{:?}",
        output.stderr_lines()
    );
    fs::remove_dir_all(vocabulary_dir)?;
    let expected = "ask not what your country can do for you";
    assert!(
        output
            .stdout_lines()
            .iter()
            .any(|line| line.to_lowercase().contains(expected)
                && line
                    .to_lowercase()
                    .contains("ask what you can do for your country")),
        "stdout: {:?}; stderr: {:?}",
        output.stdout_lines(),
        output.stderr_lines()
    );
    Ok(())
}

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
fn at02_missing_model_returns_exit_code_2() -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-acceptance-config");
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    let missing_path = temp_dir("soundvibes-missing-model").join("missing.bin");
    write_config(
        &config_home,
        &format!("model = \"{}\"\n", missing_path.display()),
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
        stderr.contains("Parakeet model missing"),
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
    continuous_pause_case(false)
}
#[cfg(feature = "test-support")]
#[test]
fn at40_live_stream_outputs_on_pause_before_release() -> Result<(), Box<dyn Error>> {
    continuous_pause_case(true)
}
#[cfg(feature = "test-support")]
fn continuous_pause_case(live: bool) -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
    let control_sender = sender.clone();
    let (transcript_sender, transcript_receiver) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TranscriptSignalOutput {
        errors: Vec::new(),
        lines: Vec::new(),
        transcript_sender,
        transcript_marker: "Transcript 1: pause transcript",
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160], vec![0.0; 400]],
        )),
        transcriber_factory: if live {
            Box::new(LiveOnlyFactory("pause transcript"))
        } else {
            Box::new(TestTranscriberFactory::new(vec!["pause transcript".into()]))
        },
    };
    let config = DaemonConfig {
        engine: if live {
            sv::inference::InferenceEngine::Parakeet
        } else {
            sv::inference::InferenceEngine::Phonon2
        },
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
    continuous_duration_case(false)
}
#[cfg(feature = "test-support")]
#[test]
fn at40_live_stream_outputs_during_long_speech() -> Result<(), Box<dyn Error>> {
    continuous_duration_case(true)
}
#[cfg(feature = "test-support")]
fn continuous_duration_case(live: bool) -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
    let control_sender = sender.clone();
    let (transcript_sender, transcript_receiver) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = TranscriptSignalOutput {
        errors: Vec::new(),
        lines: Vec::new(),
        transcript_sender,
        transcript_marker: "Transcript 1: timed transcript",
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 320], vec![0.2; 320], vec![0.2; 320]],
        )),
        transcriber_factory: if live {
            Box::new(LiveOnlyFactory("timed transcript"))
        } else {
            Box::new(TestTranscriberFactory::new(vec!["timed transcript".into()]))
        },
    };
    let config = DaemonConfig {
        engine: if live {
            sv::inference::InferenceEngine::Parakeet
        } else {
            sv::inference::InferenceEngine::Phonon2
        },
        sample_rate: 16_000,
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
    errors: Vec<String>,
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

    fn stderr(&mut self, message: &str) {
        self.errors.push(message.into());
    }
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
        stderr_joined.contains("Parakeet backend: Vulkan"),
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
        &format!(
            "model = \"{}\"\nparakeet_backend = \"cpu\"\n",
            model_path.display()
        ),
    )?;

    let stderr_lines = run_daemon_for_logs(&config_home, &runtime_dir)?;
    let stderr_joined = stderr_lines.join("\n");
    assert!(
        stderr_joined.contains("Parakeet backend: CPU"),
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

#[test]
fn at13_universal_paste_setup_configures_soundvibes_and_ghostty() -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-universal-paste-config");
    let keyd_config_dir = temp_dir("soundvibes-keyd-config");
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("contrib")
        .join("setup-universal-paste");

    let output = Command::new("sh")
        .arg(&script)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("KEYD_CONFIG_DIR", &keyd_config_dir)
        .env("KEYD_COMMAND", "true")
        .env("SKIP_KEYD_ACTIVATION", "1")
        .env("SKIP_SOUNDVIBES_RESTART", "1")
        .output()?;

    assert!(
        output.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(config_home.join("soundvibes/config.toml"))?,
        "[output]\npaste_keys = \"shift+insert\"\nmode = \"ydotool\"\n\n[hotkey]\nkey = \"F24\"\n"
    );
    assert_eq!(
        fs::read_to_string(config_home.join("ghostty/config"))?,
        "keybind = shift+insert=paste_from_clipboard\n"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Konsole already supports Shift+Insert"),
        "expected Konsole compatibility message"
    );
    assert_eq!(
        fs::read_to_string(keyd_config_dir.join("soundvibes.conf"))?,
        "[ids]\n*\n\n[main]\nrightalt+rightcontrol = f24\n"
    );
    Ok(())
}

#[test]
fn at13_universal_paste_setup_preserves_existing_config_and_is_idempotent(
) -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-universal-paste-existing-config");
    let keyd_config_dir = temp_dir("soundvibes-keyd-existing-config");
    let soundvibes_config = config_home.join("soundvibes/config.toml");
    let ghostty_config = config_home.join("ghostty/config");
    fs::create_dir_all(
        soundvibes_config
            .parent()
            .expect("soundvibes config parent"),
    )?;
    fs::create_dir_all(ghostty_config.parent().expect("ghostty config parent"))?;
    fs::write(
        &soundvibes_config,
        "language = \"de\"\n\n[output]\nrestore_clipboard = false\npaste_keys = \"ctrl+v\"\n\n[hotkey]\nenabled = true\n",
    )?;
    fs::write(
        &ghostty_config,
        "font-size = 12\nkeybind = shift+insert=paste_from_selection\n",
    )?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("contrib")
        .join("setup-universal-paste");

    for _ in 0..2 {
        let status = Command::new("sh")
            .arg(&script)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("KEYD_CONFIG_DIR", &keyd_config_dir)
            .env("KEYD_COMMAND", "true")
            .env("SKIP_KEYD_ACTIVATION", "1")
            .env("SKIP_SOUNDVIBES_RESTART", "1")
            .status()?;
        assert!(status.success(), "setup should succeed repeatedly");
    }

    let configured = fs::read_to_string(soundvibes_config)?;
    let parsed: toml::Value = toml::from_str(&configured)?;
    assert_eq!(parsed["language"].as_str(), Some("de"));
    assert_eq!(parsed["output"]["restore_clipboard"].as_bool(), Some(false));
    assert_eq!(
        parsed["output"]["paste_keys"].as_str(),
        Some("shift+insert")
    );
    assert_eq!(parsed["output"]["mode"].as_str(), Some("ydotool"));
    assert_eq!(parsed["hotkey"]["enabled"].as_bool(), Some(true));
    assert_eq!(parsed["hotkey"]["key"].as_str(), Some("F24"));
    assert_eq!(configured.matches("paste_keys =").count(), 1);
    assert_eq!(configured.matches("mode =").count(), 1);
    assert_eq!(configured.matches("key =").count(), 1);
    assert_eq!(
        fs::read_to_string(ghostty_config)?,
        "font-size = 12\nkeybind = shift+insert=paste_from_clipboard\n"
    );
    assert_eq!(
        fs::read_to_string(keyd_config_dir.join("soundvibes.conf"))?,
        "[ids]\n*\n\n[main]\nrightalt+rightcontrol = f24\n"
    );
    Ok(())
}

#[test]
fn at13_universal_paste_setup_handles_commented_output_table() -> Result<(), Box<dyn Error>> {
    let config_home = temp_dir("soundvibes-universal-paste-commented-config");
    let keyd_config_dir = temp_dir("soundvibes-keyd-commented-config");
    let soundvibes_config = config_home.join("soundvibes/config.toml");
    fs::create_dir_all(
        soundvibes_config
            .parent()
            .expect("soundvibes config parent"),
    )?;
    fs::write(
        &soundvibes_config,
        "[output] # Dictation output\nrestore_clipboard = true\n\n[hotkey]\nenabled = true\n",
    )?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("contrib")
        .join("setup-universal-paste");

    let status = Command::new("sh")
        .arg(&script)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("KEYD_CONFIG_DIR", &keyd_config_dir)
        .env("KEYD_COMMAND", "true")
        .env("SKIP_KEYD_ACTIVATION", "1")
        .env("SKIP_SOUNDVIBES_RESTART", "1")
        .status()?;

    assert!(status.success(), "setup should accept TOML table comments");
    let configured = fs::read_to_string(soundvibes_config)?;
    let parsed: toml::Value = toml::from_str(&configured)?;
    assert_eq!(
        parsed["output"]["paste_keys"].as_str(),
        Some("shift+insert")
    );
    assert_eq!(parsed["output"]["restore_clipboard"].as_bool(), Some(true));
    assert_eq!(parsed["output"]["mode"].as_str(), Some("ydotool"));
    assert_eq!(parsed["hotkey"]["enabled"].as_bool(), Some(true));
    assert_eq!(parsed["hotkey"]["key"].as_str(), Some("F24"));
    assert_eq!(configured.matches("[output]").count(), 1);
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at14_ydotool_mode_uses_three_millisecond_daemon_client() -> Result<(), Box<dyn Error>> {
    let mut runner = TestRunner::default();
    runner.push_status(0);
    let config = OutputConfig {
        mode: sv::output::OutputMode::Ydotool,
        ..OutputConfig::default()
    };

    sv::output::output_text_with_runner("fast text", &config, &mut runner)?;

    assert_eq!(runner.commands.len(), 1);
    assert_command(
        &runner.commands[0],
        "ydotool",
        &["type", "--key-delay", "3", "--key-hold", "3", "--file", "-"],
        b"fast text",
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at15_debug_transcripts_log_exact_text_before_output() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
    let control_sender = sender.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = OrderedOutput::default();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".to_string()],
            vec![vec![0.2; 160]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
            "hello\n\"world\"".to_string()
        ])),
    };
    let config = DaemonConfig {
        debug_transcripts: true,
        ..daemon_config()
    };

    let shutdown_trigger = Arc::clone(&shutdown);
    let control_thread = thread::spawn(move || {
        let _ = control_sender.send(ControlEvent::StartRecording);
        let _ = control_sender.send(ControlEvent::StopRecording);
        thread::sleep(Duration::from_millis(50));
        shutdown_trigger.store(true, Ordering::Relaxed);
    });

    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, shutdown.as_ref())?;
    control_thread.join().expect("control thread failed");

    let debug_position = output
        .events
        .iter()
        .position(|event| event == r#"stderr:Debug transcript 1: "hello\n\"world\"""#)
        .ok_or("missing exact transcript debug line")?;
    let output_position = output
        .events
        .iter()
        .position(|event| event == "stdout:Transcript 1: hello\n\"world\"")
        .ok_or("missing transcript output line")?;
    assert!(
        debug_position < output_position,
        "expected transcript debug line before output"
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct OrderedOutput {
    events: Vec<String>,
}

#[cfg(feature = "test-support")]
impl DaemonOutput for OrderedOutput {
    fn stdout(&mut self, message: &str) {
        self.events.push(format!("stdout:{message}"));
    }

    fn stderr(&mut self, message: &str) {
        self.events.push(format!("stderr:{message}"));
    }
}

#[cfg(feature = "test-support")]
#[test]
fn at12_daemon_status_is_acknowledged() -> Result<(), Box<dyn Error>> {
    let runtime_dir = temp_dir("soundvibes-acceptance-runtime");
    fs::create_dir_all(&runtime_dir)?;
    let _runtime_guard = EnvGuard::set("XDG_RUNTIME_DIR", &runtime_dir);
    let data_dir = temp_dir("soundvibes-acceptance-data");
    fs::create_dir_all(&data_dir)?;
    let _data_guard = EnvGuard::set("XDG_DATA_HOME", &data_dir);
    let socket_path = sv::daemon::daemon_socket_path()?;
    let (_socket_guard, receiver, _sender) = sv::daemon::start_socket_listener(&socket_path)?;

    let client = thread::spawn(|| -> Result<(), AppError> {
        let status = sv::daemon::send_status_command()?;
        assert!(status.ok);
        assert_eq!(status.state.as_deref(), Some("idle"));
        assert_eq!(status.language.as_deref(), Some("en"));

        let stopped = sv::daemon::send_stop_command()?;
        assert!(stopped.ok);
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
    if let Ok(path) = env::var("SV_PARAKEET_MODEL") {
        return Ok(PathBuf::from(path));
    }
    let data_home = env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    Ok(data_home
        .join("soundvibes")
        .join("models")
        .join("parakeet-unified-en-0.6b-Q8_0.gguf"))
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

// AT-38: explicit assets make missing/invalid models fail instead of silently skipping.
#[test]
fn parakeet_real_asset_offline() -> Result<(), Box<dyn Error>> {
    let Some(model) = env::var_os("SV_PARAKEET_MODEL") else {
        eprintln!("skip AT-38: set SV_PARAKEET_MODEL to a verified Unified EN GGUF");
        return Ok(());
    };
    use sv::daemon::Transcriber;
    let backend = if env::var("SV_PARAKEET_BACKEND").as_deref() == Ok("vulkan") {
        sv::parakeet::ParakeetBackend::Vulkan
    } else {
        sv::parakeet::ParakeetBackend::Cpu
    };
    let mut transcriber = sv::parakeet::ParakeetTranscriber::load(&PathBuf::from(model), backend)?;
    transcriber.set_vocabulary(&["TanStack".into(), "shadcn".into()])?;
    let mut wav = hound::WavReader::open("tests/fixtures/jfk.wav")?;
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    // Cancel an active native session, then reuse its model/session offline.
    {
        let mut stream = transcriber.begin_stream(Some("en"))?.expect("live session");
        stream.feed(&samples[..1600])?;
    }
    let text = transcriber.transcribe(&samples, Some("en"))?.to_lowercase();
    assert!(
        text.contains("country") && text.contains("americans"),
        "{text}"
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn parakeet_real_asset_streams_through_daemon() -> Result<(), Box<dyn Error>> {
    let Some(model) = env::var_os("SV_PARAKEET_MODEL") else {
        eprintln!("skip AT-39: set SV_PARAKEET_MODEL");
        return Ok(());
    };
    let mut wav = hound::WavReader::open("tests/fixtures/jfk.wav")?;
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    let directory = temp_dir("parakeet-stream-vocabulary");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("app.txt"), "TanStack\nshadcn\n")?;
    let config = DaemonConfig {
        engine: sv::inference::InferenceEngine::Parakeet,
        model_path: Some(model.into()),
        vocabulary_dir: Some(directory.clone()),
        parakeet_backend: if env::var("SV_PARAKEET_BACKEND").as_deref() == Ok("vulkan") {
            sv::parakeet::ParakeetBackend::Vulkan
        } else {
            sv::parakeet::ParakeetBackend::Cpu
        },
        vad: VadMode::Continuous,
        ..daemon_config()
    };
    let mut deps = DaemonDeps::for_config(&config)?;
    deps.audio = Box::new(TestAudioBackend::new(
        vec!["Fixture".into()],
        vec![samples[..1600].to_vec(), samples[1600..].to_vec()],
    ));
    let (sender, receiver) = mpsc::channel();
    let controller = thread::spawn(move || -> Result<(), String> {
        acknowledge_with_timeout(
            &sender,
            ControlEvent::StartRecording,
            Duration::from_secs(300),
        )?;
        // First chunk drains in the start tick; finish drains the entire remainder.
        acknowledge_with_timeout(
            &sender,
            ControlEvent::StopRecording,
            Duration::from_secs(300),
        )?;
        acknowledge_with_timeout(&sender, ControlEvent::Stop, Duration::from_secs(300))
    });
    let mut output = TestOutput::default();
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    controller.join().unwrap()?;
    fs::remove_dir_all(directory)?;
    assert!(
        !output
            .stderr_lines()
            .iter()
            .any(|line| line.contains("using offline")),
        "{:?}",
        output.stderr_lines()
    );
    let lines = output.stdout_lines().join(" ").to_lowercase();
    assert!(
        lines.contains("country") && lines.contains("americans"),
        "{lines}"
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at40_live_timed_overlap_is_fed_and_deduped_on_release() -> Result<(), Box<dyn Error>> {
    struct Factory(mpsc::Sender<()>);
    struct Recognizer {
        sequence: usize,
        tail_ready: mpsc::Sender<()>,
    }
    struct Session {
        sequence: usize,
        audio: Vec<f32>,
        tail_ready: mpsc::Sender<()>,
    }
    impl sv::daemon::TranscriberFactory for Factory {
        fn load(
            &self,
            _: Option<&std::path::Path>,
        ) -> Result<Box<dyn sv::daemon::Transcriber>, AppError> {
            Ok(Box::new(Recognizer {
                sequence: 0,
                tail_ready: self.0.clone(),
            }))
        }
    }
    impl sv::daemon::Transcriber for Recognizer {
        fn set_vocabulary(&mut self, _: &[String]) -> Result<Option<String>, AppError> {
            Ok(None)
        }
        fn transcribe(&self, _: &[f32], _: Option<&str>) -> Result<String, AppError> {
            Err(AppError::runtime("unexpected offline fallback"))
        }
        fn begin_stream(
            &mut self,
            _: Option<&str>,
        ) -> Result<Option<Box<dyn sv::daemon::StreamingTranscriber + '_>>, AppError> {
            self.sequence += 1;
            Ok(Some(Box::new(Session {
                sequence: self.sequence,
                audio: Vec::new(),
                tail_ready: self.tail_ready.clone(),
            })))
        }
    }
    impl sv::daemon::StreamingTranscriber for Session {
        fn feed(&mut self, samples: &[f32]) -> Result<(), AppError> {
            self.audio.extend_from_slice(samples);
            if self.sequence == 2 && self.audio.len() >= 320 {
                let _ = self.tail_ready.send(());
            }
            Ok(())
        }
        fn finish(&mut self) -> Result<String, AppError> {
            let (expected, text) = if self.sequence == 1 {
                ([vec![0.2; 320], vec![0.3; 320]].concat(), "hello world")
            } else {
                ([vec![0.3; 160], vec![0.4; 160]].concat(), "world again")
            };
            if self.audio != expected {
                return Err(AppError::runtime("incorrect streamed audio at boundary"));
            }
            Ok(text.into())
        }
    }
    let (tail_sender, tail_receiver) = mpsc::channel();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Fixture".into()],
            vec![vec![0.2; 320], vec![0.3; 320], vec![0.4; 160]],
        )),
        transcriber_factory: Box::new(Factory(tail_sender)),
    };
    let config = DaemonConfig {
        engine: sv::inference::InferenceEngine::Parakeet,
        vad: VadMode::Continuous,
        segment_target_ms: 20,
        segment_grace_ms: 20,
        segment_overlap_ms: 10,
        segment_min_ms: 10,
        ..daemon_config()
    };
    let (sender, receiver) = mpsc::channel();
    let controller = thread::spawn(move || -> Result<(), String> {
        acknowledge(&sender, ControlEvent::StartRecording)?;
        let fed = tail_receiver.recv_timeout(Duration::from_secs(3));
        acknowledge(&sender, ControlEvent::StopRecording)?;
        acknowledge(&sender, ControlEvent::Stop)?;
        fed.map_err(|e| format!("second stream never received its overlap and new audio: {e}"))
    });
    let mut output = TestOutput::default();
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    controller.join().unwrap()?;
    assert!(
        output.stderr_lines().is_empty(),
        "{:?}",
        output.stderr_lines()
    );
    let transcripts: Vec<_> = output
        .stdout_lines()
        .iter()
        .filter(|line| line.starts_with("Transcript "))
        .cloned()
        .collect();
    assert_eq!(
        transcripts,
        ["Transcript 1: hello world", "Transcript 2: again"]
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at40_real_parakeet_outputs_on_pause_and_duration_before_release() -> Result<(), Box<dyn Error>> {
    let Some(model) = env::var_os("SV_PARAKEET_MODEL") else {
        eprintln!("skip AT-40 native cases: set SV_PARAKEET_MODEL");
        return Ok(());
    };
    let mut wav = hound::WavReader::open("tests/fixtures/jfk.wav")?;
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    for timed in [false, true] {
        let config = DaemonConfig {
            engine: sv::inference::InferenceEngine::Parakeet,
            model_path: Some(PathBuf::from(&model)),
            parakeet_backend: if env::var("SV_PARAKEET_BACKEND").as_deref() == Ok("vulkan") {
                sv::parakeet::ParakeetBackend::Vulkan
            } else {
                sv::parakeet::ParakeetBackend::Cpu
            },
            vad: VadMode::Continuous,
            vad_silence_ms: 100,
            vad_chunk_ms: 20,
            segment_target_ms: if timed { 1000 } else { 60000 },
            segment_grace_ms: 1000,
            ..daemon_config()
        };
        let mut deps = DaemonDeps::for_config(&config)?;
        let mut chunks = vec![samples.clone()];
        if !timed {
            chunks.push(vec![0.0; 6400]);
        }
        deps.audio = Box::new(TestAudioBackend::new(vec!["Fixture".into()], chunks));
        let (sender, receiver) = mpsc::channel();
        let (transcript_sender, transcript_receiver) = mpsc::channel();
        let controller = thread::spawn(move || -> Result<(), String> {
            acknowledge_with_timeout(
                &sender,
                ControlEvent::StartRecording,
                Duration::from_secs(300),
            )?;
            let seen = transcript_receiver.recv_timeout(Duration::from_secs(120));
            acknowledge_with_timeout(
                &sender,
                ControlEvent::StopRecording,
                Duration::from_secs(300),
            )?;
            acknowledge_with_timeout(&sender, ControlEvent::Stop, Duration::from_secs(300))?;
            seen.map_err(|e| {
                format!("native segment never emitted before release, timed={timed}: {e}")
            })
        });
        let mut output = TranscriptSignalOutput {
            errors: Vec::new(),
            lines: Vec::new(),
            transcript_sender,
            transcript_marker: "Transcript 1:",
        };
        sv::daemon::run_daemon_loop(
            &config,
            &deps,
            &mut output,
            receiver,
            &AtomicBool::new(false),
        )?;
        controller.join().unwrap()?;
        assert!(
            output.errors.is_empty(),
            "timed={timed}: {:?}",
            output.errors
        );
        let lines = output.stdout_lines().join(" ").to_lowercase();
        assert!(
            lines.contains("country") && lines.contains("americans"),
            "timed={timed}: {lines}"
        );
    }
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn at40_live_pause_after_output_does_not_cancel_the_hold() -> Result<(), Box<dyn Error>> {
    let audio = TestAudioBackend::new(vec!["Fixture".into()], Vec::new());
    let producer = audio.clone();
    let deps = DaemonDeps {
        audio: Box::new(audio),
        transcriber_factory: Box::new(LiveOnlyFactory("spoken segment")),
    };
    let config = DaemonConfig {
        engine: sv::inference::InferenceEngine::Parakeet,
        vad: VadMode::Continuous,
        no_speech_timeout_ms: 200,
        vad_silence_ms: 20,
        vad_chunk_ms: 20,
        segment_min_ms: 20,
        ..daemon_config()
    };
    let (sender, receiver) = mpsc::channel();
    let (transcript_sender, transcript_receiver) = mpsc::channel();
    let controller = thread::spawn(move || -> Result<(), String> {
        acknowledge(&sender, ControlEvent::StartRecording)?;
        let speak = || {
            producer.push_samples(vec![0.2; 1600]);
            producer.push_samples(vec![0.0; 320]);
        };
        speak();
        transcript_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        thread::sleep(Duration::from_millis(350));
        speak();
        let resumed = transcript_receiver.recv_timeout(Duration::from_secs(2));
        acknowledge(&sender, ControlEvent::StopRecording)?;
        acknowledge(&sender, ControlEvent::Stop)?;
        resumed.map_err(|e| format!("resumed speech was lost while the hotkey stayed held: {e}"))
    });
    let mut output = TranscriptSignalOutput {
        errors: Vec::new(),
        lines: Vec::new(),
        transcript_sender,
        transcript_marker: ": spoken segment",
    };
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    controller.join().unwrap()?;
    assert!(
        !output.lines.iter().any(|line| line.contains("cancelled")),
        "{:?}",
        output.lines
    );
    assert!(output.errors.is_empty(), "{:?}", output.errors);
    Ok(())
}

// AT-41: The desktop tray follows capture state and retains recent output.
#[cfg(feature = "test-support")]
#[test]
fn tray_tracks_capture_and_recent_transcripts() -> Result<(), Box<dyn Error>> {
    use ksni::Tray;
    let (sender, receiver) = mpsc::channel();
    sender.send(ControlEvent::StartRecording)?;
    sender.send(ControlEvent::StopRecording)?;
    sender.send(ControlEvent::Stop)?;
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".into()],
            vec![vec![0.2; 160]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec!["hello_world".into()])),
    };
    let mut output = DesktopOutput::default();
    sv::daemon::run_daemon_loop(
        &daemon_config(),
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    assert_eq!(
        output.states,
        [
            Activity::Idle,
            Activity::Listening,
            Activity::Processing,
            Activity::Idle
        ]
    );
    assert_eq!(
        output.tray.recent_transcripts().collect::<Vec<_>>(),
        ["hello_world"]
    );
    assert_eq!(output.tray.title(), "SoundVibes: Idle");
    Ok(())
}

// AT-41: History is bounded, newest first, and preserves literal transcript text.
#[test]
fn tray_history_keeps_the_latest_ten_transcripts() {
    use sv::tray::TranscriptTray;

    let mut tray = TranscriptTray::default();
    tray.append_segment(" \n ");
    tray.finish_recording();
    assert!(tray.history_popup(0, 0).content.transcripts.is_empty());
    tray.push_transcript("discarded");
    for text in [
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten_&_十\nnext line",
    ] {
        tray.push_transcript(text);
    }
    tray.push_transcript(" \n ");
    assert_eq!(
        tray.recent_transcripts().collect::<Vec<_>>(),
        [
            "ten_&_十\nnext line",
            "nine",
            "eight",
            "seven",
            "six",
            "five",
            "four",
            "three",
            "two",
            "one"
        ]
    );
    assert_eq!(
        tray.history_popup(0, 0).content.transcripts[0],
        "ten_&_十\nnext line"
    );
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct DesktopOutput {
    tray: sv::tray::TranscriptTray,
    states: Vec<Activity>,
}
#[cfg(feature = "test-support")]
impl DaemonOutput for DesktopOutput {
    fn stdout(&mut self, _: &str) {}
    fn stderr(&mut self, _: &str) {}
    fn activity_changed(&mut self, activity: Activity) {
        self.states.push(activity);
        self.tray.set_activity(activity);
    }
    fn transcript(&mut self, text: &str) {
        self.tray.append_segment(text);
    }
    fn recording_finished(&mut self) {
        self.tray.finish_recording();
    }
}

#[cfg(feature = "test-support")]
#[test]
fn tray_stays_idle_after_capture_failure() {
    let (sender, receiver) = mpsc::channel();
    sender.send(ControlEvent::StartRecording).unwrap();
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::with_start_error(
            vec!["Mic".into()],
            sv::audio::AudioError {
                kind: sv::audio::AudioErrorKind::DeviceUnavailable,
                message: "capture unavailable".into(),
            },
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![])),
    };
    let mut output = DesktopOutput::default();
    assert!(sv::daemon::run_daemon_loop(
        &daemon_config(),
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(output.states, [Activity::Idle]);
    assert_eq!(output.tray.recent_transcripts().count(), 0);
}

#[cfg(feature = "test-support")]
#[test]
fn tray_returns_to_idle_after_silence_cancellation() -> Result<(), Box<dyn Error>> {
    struct CancelOutput {
        desktop: DesktopOutput,
        shutdown: Arc<AtomicBool>,
        cancelled: bool,
    }
    impl DaemonOutput for CancelOutput {
        fn stdout(&mut self, text: &str) {
            self.cancelled |= text == "No speech detected, cancelled.";
        }
        fn stderr(&mut self, _: &str) {}
        fn activity_changed(&mut self, activity: Activity) {
            self.desktop.activity_changed(activity);
            if self.desktop.states.len() == 3 {
                self.shutdown.store(true, Ordering::Relaxed);
            }
        }
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut output = CancelOutput {
        desktop: DesktopOutput::default(),
        shutdown: shutdown.clone(),
        cancelled: false,
    };
    let (sender, receiver) = mpsc::channel();
    sender.send(ControlEvent::StartRecording)?;
    let (done_sender, done_receiver) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if done_receiver.recv_timeout(Duration::from_secs(1)).is_err() {
            let _ = sender.send(ControlEvent::Stop);
        }
    });
    let config = DaemonConfig {
        no_speech_timeout_ms: 1,
        ..daemon_config()
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Mic".into()],
            vec![vec![0.0; 160]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![])),
    };
    sv::daemon::run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown)?;
    let _ = done_sender.send(());
    watchdog.join().expect("tray test watchdog");
    assert!(
        output.cancelled,
        "silent capture should cancel before the watchdog stops the daemon"
    );
    assert_eq!(
        output.desktop.states,
        [Activity::Idle, Activity::Listening, Activity::Idle]
    );
    Ok(())
}

// AT-41: Clicking a history row copies its complete, unescaped text.
#[cfg(feature = "test-support")]
#[test]
fn tray_click_copies_the_selected_transcript() {
    use sv::output::OutputMode;

    let runner = Arc::new(Mutex::new(TestRunner::default()));
    runner.lock().unwrap().push_status(0);
    let clipboard_runner = runner.clone();
    let mut tray = sv::tray::TranscriptTray::with_clipboard(move |text| {
        sv::output::output_text_with_runner(
            text,
            &OutputConfig {
                mode: OutputMode::Clipboard,
                ..OutputConfig::default()
            },
            &mut *clipboard_runner.lock().unwrap(),
        )
        .unwrap();
    });
    tray.push_transcript("Older transcript");
    tray.push_transcript("Full_Unicode 十\nsecond line");
    let snapshot = tray.history_popup(0, 0);
    tray.push_transcript("New recording after the popup opened");
    assert!(snapshot.copy_selected(0));
    assert!(!snapshot.copy_selected(100));
    let commands = &runner.lock().unwrap().commands;
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].program, "wl-copy");
    assert_eq!(commands[0].stdin, "Full_Unicode 十\nsecond line".as_bytes());
}

// AT-41: A history copy waits for the complete dictation paste/restore transaction.
#[cfg(feature = "test-support")]
#[test]
fn tray_copy_waits_for_dictation_clipboard_transaction() {
    use sv::output::{output_text_with_runner, CommandRunner, OutputError, OutputMode};

    struct PausingRunner {
        inner: TestRunner,
        paused: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
        pause_at: Duration,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl CommandRunner for PausingRunner {
        fn output(
            &mut self,
            program: &str,
            args: &[String],
        ) -> std::io::Result<std::process::Output> {
            self.inner.output(program, args)
        }
        fn status_with_stdin(
            &mut self,
            program: &str,
            args: &[String],
            stdin: &[u8],
        ) -> std::io::Result<std::process::ExitStatus> {
            self.events.lock().unwrap().push(program.to_string());
            self.inner.status_with_stdin(program, args, stdin)
        }
        fn copy_temporary_text(&mut self, text: &str) -> Result<(), OutputError> {
            self.inner.copy_temporary_text(text)
        }
        fn sleep(&mut self, duration: Duration) {
            if duration == self.pause_at {
                self.paused.send(()).unwrap();
                self.resume.recv_timeout(Duration::from_secs(2)).unwrap();
            }
        }
    }
    for pause_ms in [100, 250] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (paused_tx, paused_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let mut inner = TestRunner::default();
        inner.push_output(0, b"text/plain\n", b"");
        inner.push_output(0, b"old clipboard", b"");
        inner.push_status(0);
        inner.push_status(0);
        let mut runner = PausingRunner {
            inner,
            paused: paused_tx,
            resume: resume_rx,
            pause_at: Duration::from_millis(pause_ms),
            events: events.clone(),
        };
        let paste = thread::spawn(move || {
            output_text_with_runner("dictation", &OutputConfig::default(), &mut runner).unwrap();
        });
        paused_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (copied_tx, copied_rx) = mpsc::channel();
        let copy_events = events.clone();
        let copy = thread::spawn(move || {
            let mut tray = sv::tray::TranscriptTray::with_clipboard(move |text| {
                let mut runner = TestRunner::default();
                runner.push_status(0);
                output_text_with_runner(
                    text,
                    &OutputConfig {
                        mode: OutputMode::Clipboard,
                        ..OutputConfig::default()
                    },
                    &mut runner,
                )
                .unwrap();
                copy_events.lock().unwrap().push("history copy".into());
                copied_tx.send(()).unwrap();
            });
            tray.push_transcript("history");
            let snapshot = tray.history_popup(0, 0);
            attempt_tx.send(()).unwrap();
            assert!(snapshot.copy_selected(0));
        });
        attempt_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let copied_early = copied_rx.recv_timeout(Duration::from_millis(30)).is_ok();
        resume_tx.send(()).unwrap();
        paste.join().unwrap();
        copy.join().unwrap();
        assert!(
            !copied_early,
            "history copy interrupted the paste transaction at {pause_ms} ms"
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["dotool", "wl-copy", "history copy"]
        );
    }
}

// AT-41: Continuous segments become one clickable history entry per recording.
#[cfg(feature = "test-support")]
#[test]
fn tray_history_groups_segments_until_recording_finishes() -> Result<(), Box<dyn Error>> {
    struct RecordingOutput {
        desktop: DesktopOutput,
        segments: mpsc::Sender<()>,
        history_during_segments: Vec<usize>,
        activity_during_segments: Vec<Activity>,
    }
    impl DaemonOutput for RecordingOutput {
        fn stdout(&mut self, _: &str) {}
        fn stderr(&mut self, _: &str) {}
        fn activity_changed(&mut self, activity: Activity) {
            self.desktop.activity_changed(activity);
        }
        fn transcript(&mut self, text: &str) {
            self.desktop.transcript(text);
            self.activity_during_segments
                .push(*self.desktop.states.last().unwrap());
            self.history_during_segments
                .push(self.desktop.tray.recent_transcripts().count());
            let _ = self.segments.send(());
        }
        fn recording_finished(&mut self) {
            self.desktop.recording_finished();
        }
    }
    let (copy_tx, copy_rx) = mpsc::channel();
    let (segment_tx, segment_rx) = mpsc::channel();
    let mut output = RecordingOutput {
        desktop: DesktopOutput {
            tray: sv::tray::TranscriptTray::with_clipboard(move |text| {
                copy_tx.send(text.to_string()).unwrap();
            }),
            ..DesktopOutput::default()
        },
        segments: segment_tx,
        history_during_segments: vec![],
        activity_during_segments: vec![],
    };
    let audio = TestAudioBackend::new(vec!["Mic".into()], vec![vec![0.2; 320], vec![0.0; 400]]);
    let control_audio = audio.clone();
    let (sender, receiver) = mpsc::channel();
    sender.send(ControlEvent::StartRecording)?;
    let control = thread::spawn(move || {
        let first_segment = segment_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        if first_segment {
            control_audio.push_samples(vec![0.2; 320]);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        sender
            .send(ControlEvent::Request {
                event: Box::new(ControlEvent::StopRecording),
                respond_to: reply_tx,
            })
            .unwrap();
        let reply = reply_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(reply.ok);
        control_audio.push_samples(vec![0.2; 160]);
        sender.send(ControlEvent::StartRecording).unwrap();
        sender.send(ControlEvent::Stop).unwrap();
        first_segment
    });
    let config = DaemonConfig {
        vad: VadMode::Continuous,
        vad_silence_ms: 20,
        vad_chunk_ms: 20,
        segment_min_ms: 20,
        ..daemon_config()
    };
    let deps = DaemonDeps {
        audio: Box::new(audio),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
            "First part.".into(),
            "Last_words 十.".into(),
            "Separate recording.".into(),
        ])),
    };
    sv::daemon::run_daemon_loop(
        &config,
        &deps,
        &mut output,
        receiver,
        &AtomicBool::new(false),
    )?;
    assert!(
        control.join().unwrap(),
        "expected a segment before releasing the recording"
    );
    assert_eq!(output.history_during_segments, [0, 0, 1]);
    assert_eq!(
        output.activity_during_segments,
        [
            Activity::Listening,
            Activity::Processing,
            Activity::Processing
        ]
    );
    assert_eq!(output.desktop.states.last(), Some(&Activity::Idle));
    assert_eq!(
        output.desktop.tray.recent_transcripts().collect::<Vec<_>>(),
        ["Separate recording.", "First part. Last_words 十."]
    );
    assert!(output.desktop.tray.history_popup(0, 0).copy_selected(1));
    assert_eq!(
        copy_rx.recv_timeout(Duration::from_secs(1))?,
        "First part. Last_words 十."
    );
    Ok(())
}

// AT-41: Dedicated recording dots distinguish idle, listening and processing.
#[test]
fn tray_uses_distinct_recording_dots_for_each_activity() {
    use ksni::Tray;
    let mut tray = sv::tray::TranscriptTray::with_clipboard(|_| {});
    let mut centers = Vec::new();
    for (activity, title) in [
        (Activity::Idle, "SoundVibes: Idle"),
        (Activity::Listening, "SoundVibes: Listening"),
        (Activity::Processing, "SoundVibes: Processing"),
    ] {
        tray.set_activity(activity);
        assert_eq!(tray.title(), title);
        assert!(
            tray.icon_name().is_empty(),
            "use the recording dot, not a theme audio icon"
        );
        let icons = tray.icon_pixmap();
        let icon = &icons[0];
        assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        assert_eq!(icon.data[0], 0, "circle corners must be transparent");
        let center = ((icon.height / 2 * icon.width + icon.width / 2) * 4) as usize;
        centers.push(icon.data[center..center + 4].to_vec());
        assert!(!tray.tool_tip().icon_pixmap.is_empty());
    }
    assert_eq!(centers[0][0], 255);
    assert_eq!(centers[0][1], centers[0][2]);
    assert_eq!(centers[0][2], centers[0][3]);
    assert!(centers[1][1] > centers[1][2] * 2, "listening should be red");
    assert!(
        centers[2][1] > centers[2][2] && centers[2][2] > centers[2][3],
        "processing should be amber"
    );
    assert_ne!(centers[1], centers[2]);
}

// AT-42: The real Qt popup wraps full recordings at a fixed, scrollable width.
#[cfg(feature = "test-support")]
#[test]
fn history_popup_wraps_and_selects_full_recordings() -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    let text = format!(
        "A complete recording with <literal markup> and Unicode 十. {}\n{}",
        "Words that should wrap naturally. ".repeat(12),
        "long_unbroken_token_".repeat(30)
    );
    let transcripts = vec![text.clone(); 10];
    let mut child = Command::new(env!("CARGO_BIN_EXE_sv"))
        .arg("tray-popup-inspect")
        .args(["--select", "0"])
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(
        serde_json::to_string(
            &serde_json::json!({"title":"SoundVibes: Idle", "transcripts":transcripts,"x":0,"y":0}),
        )?
        .as_bytes(),
    )?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["width"], 420);
    assert!(report["height"].as_i64().unwrap() <= 560);
    assert_eq!(report["horizontal_scroll_max"], 0);
    assert!(report["vertical_scroll_max"].as_i64().unwrap() > 0);
    assert_eq!(report["rows"].as_array().unwrap().len(), 10);
    assert_eq!(report["rows"][0]["text"], text);
    assert!(
        report["rows"][0]["height"].as_i64().unwrap()
            > report["rows"][0]["line_height"].as_i64().unwrap() * 3
    );
    assert!(
        report["rows"][0]["height"].as_i64().unwrap()
            >= report["rows"][0]["required_height"].as_i64().unwrap()
    );
    assert_eq!(report["rows"][0]["label_visible"], true);
    assert!(
        report["rows"][0]["label_height"].as_i64().unwrap()
            >= report["rows"][0]["label_required_height"].as_i64().unwrap()
    );
    assert_eq!(report["selected"], 0);
    Ok(())
}

// AT-42: Empty history is compact and dismissal does not select a recording.
#[cfg(feature = "test-support")]
#[test]
fn history_popup_empty_state_has_no_selection() -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_sv"))
        .arg("tray-popup-inspect")
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"title":"SoundVibes: Idle","transcripts":[],"x":0,"y":0}"#)?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["selected"], -1);
    assert_eq!(report["rows"].as_array().unwrap().len(), 0);
    assert!(report["height"].as_i64().unwrap() < 250);
    assert_eq!(report["vertical_scroll_max"], 0);
    Ok(())
}
