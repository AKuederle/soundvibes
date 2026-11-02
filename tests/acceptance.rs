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
fn at16a_phonon2_missing_assets_report_setup_without_whisper_download() -> Result<(), Box<dyn Error>>
{
    let config_home = temp_dir("soundvibes-phonon2-config");
    let model_dir = temp_dir("soundvibes-phonon2-model");
    fs::create_dir_all(&model_dir)?;
    write_config(&config_home, "engine = \"phonon2\"\n")?;
    let output = Command::new(env!("CARGO_BIN_EXE_sv"))
        .args(["daemon", "start", "--download-model", "true", "--model"])
        .arg(&model_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("SV_MODEL_BASE_URL", "http://127.0.0.1:1")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Phonon-2 model not found") && stderr.contains("prepare-phonon2.py"),
        "{stderr}"
    );
    assert!(!model_dir.join("ggml-small.bin").exists());
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
    let mut reader = hound::WavReader::open("vendor/whisper.cpp/samples/jfk.wav")?;
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

#[cfg(feature = "test-support")]
#[test]
fn at16c_phonon2_rejects_whisper_reload_without_interrupting_recording(
) -> Result<(), Box<dyn Error>> {
    let config = DaemonConfig {
        engine: sv::inference::InferenceEngine::Phonon2,
        ..daemon_config()
    };
    let deps = DaemonDeps {
        audio: Box::new(TestAudioBackend::new(
            vec!["Fixture".into()],
            vec![vec![0.2; 1600]],
        )),
        transcriber_factory: Box::new(TestTranscriberFactory::new(vec!["still recording".into()])),
    };
    let (sender, receiver) = mpsc::channel();
    let (reply, response) = mpsc::sync_channel(1);
    sender.send(ControlEvent::StartRecording)?;
    sender.send(ControlEvent::Request {
        event: Box::new(ControlEvent::SetModel {
            size: ModelSize::Tiny,
            model_language: ModelLanguage::En,
        }),
        respond_to: reply,
    })?;
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
    let response = response.recv_timeout(Duration::from_secs(1))?;
    assert!(!response.ok);
    assert!(response.message.unwrap_or_default().contains("set-model"));
    assert!(
        output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 1: still recording")),
        "{:?}",
        output.stdout_lines()
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

// AT-38: explicit assets make missing/invalid models fail instead of silently skipping.
#[test]
fn parakeet_real_asset_offline() -> Result<(), Box<dyn Error>> {
    let Some(model) = env::var_os("SV_PARAKEET_MODEL") else {
        eprintln!("skip AT-38: set SV_PARAKEET_MODEL to a verified Unified EN GGUF");
        return Ok(());
    };
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_sv")).with_file_name("sv-parakeet");
    let mut child = Command::new(helper)
        .arg(model)
        .arg(env::var("SV_PARAKEET_BACKEND").unwrap_or_else(|_| "cpu".into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    assert!(
        output.read_line(&mut line)? > 0,
        "companion failed loading model"
    );
    let mut wav = hound::WavReader::open("vendor/whisper.cpp/samples/jfk.wav")?;
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    let mut input = child.stdin.take().unwrap();
    writeln!(
        input,
        "{}",
        serde_json::json!({"op":"offline", "samples":samples, "vocabulary":["TanStack", "shadcn"]})
    )?;
    line.clear();
    output.read_line(&mut line)?;
    let reply: serde_json::Value = serde_json::from_str(&line)?;
    let text = reply["text"]
        .as_str()
        .expect("successful offline transcript")
        .to_lowercase();
    assert!(
        text.contains("country") && text.contains("americans"),
        "{text}"
    );
    drop(input);
    assert!(child.wait()?.success());
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn parakeet_real_asset_streams_through_daemon() -> Result<(), Box<dyn Error>> {
    let Some(model) = env::var_os("SV_PARAKEET_MODEL") else {
        eprintln!("skip AT-39: set SV_PARAKEET_MODEL");
        return Ok(());
    };
    let mut wav = hound::WavReader::open("vendor/whisper.cpp/samples/jfk.wav")?;
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
