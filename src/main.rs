use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use sv::audio;
use sv::daemon;
use sv::error::AppError;
use sv::hotkey::HotkeyConfig;
use sv::model::{model_language_for_transcription, ModelLanguage, ModelSize, ModelSpec};
use sv::output::{OutputConfig, OutputMode};
use sv::segmentation::{
    DEFAULT_SEGMENT_GRACE_MS, DEFAULT_SEGMENT_MIN_MS, DEFAULT_SEGMENT_OVERLAP_MS,
    DEFAULT_SEGMENT_TARGET_MS,
};
use sv::types::{AudioHost, OutputFormat, VadMode, VadSetting};

#[derive(Parser, Debug)]
#[command(name = "sv", version, about = "Offline speech-to-text CLI")]
struct Cli {
    #[arg(long, value_name = "PATH", global = true)]
    model: Option<PathBuf>,

    #[arg(long, default_value = "small", value_name = "SIZE", global = true)]
    model_size: ModelSize,

    #[arg(long, default_value = "auto", value_name = "LANG", global = true)]
    model_language: ModelLanguage,

    #[arg(long, default_value = "en", value_name = "CODE", global = true)]
    language: String,

    #[arg(long, value_name = "NAME", global = true)]
    device: Option<String>,

    #[arg(long, value_name = "HOST", global = true)]
    audio_host: Option<AudioHost>,

    #[arg(long, default_value_t = 16_000, value_name = "HZ", global = true)]
    sample_rate: u32,

    #[arg(long, default_value = "plain", value_name = "MODE", global = true)]
    format: OutputFormat,

    #[arg(long, default_value = "paste", value_name = "MODE", global = true)]
    mode: OutputMode,

    #[arg(long, default_value = "ctrl+v", value_name = "KEYS", global = true)]
    paste_keys: String,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, global = true)]
    restore_clipboard: bool,

    #[arg(long, default_value_t = 100, value_name = "MS", global = true)]
    pre_paste_delay_ms: u64,

    #[arg(long, default_value_t = 250, value_name = "MS", global = true)]
    restore_clipboard_delay_ms: u64,

    #[arg(long, default_value = "on", value_name = "MODE", global = true)]
    vad: VadMode,

    #[arg(
        long,
        default_value_t = audio::DEFAULT_SILENCE_TIMEOUT_MS,
        value_name = "MS",
        global = true
    )]
    vad_silence_ms: u64,

    #[arg(
        long,
        default_value_t = audio::DEFAULT_VAD_THRESHOLD,
        value_name = "LEVEL",
        global = true
    )]
    vad_threshold: f32,

    #[arg(
        long,
        default_value_t = audio::DEFAULT_CHUNK_MS,
        value_name = "MS",
        global = true
    )]
    vad_chunk_ms: u64,

    #[arg(long, default_value_t = DEFAULT_SEGMENT_TARGET_MS, value_name = "MS", global = true)]
    segment_target_ms: u64,

    #[arg(long, default_value_t = DEFAULT_SEGMENT_GRACE_MS, value_name = "MS", global = true)]
    segment_grace_ms: u64,

    #[arg(long, default_value_t = DEFAULT_SEGMENT_OVERLAP_MS, value_name = "MS", global = true)]
    segment_overlap_ms: u64,

    #[arg(long, default_value_t = DEFAULT_SEGMENT_MIN_MS, value_name = "MS", global = true)]
    segment_min_ms: u64,

    #[arg(long, default_value_t = false, global = true)]
    debug_audio: bool,

    #[arg(long, default_value_t = false, global = true)]
    debug_vad: bool,

    #[arg(long, default_value_t = false, global = true)]
    list_devices: bool,

    #[arg(long, default_value_t = false, global = true)]
    dump_audio: bool,

    #[arg(long, default_value_t = false, global = true)]
    audio_feedback: bool,

    #[arg(long, default_value_t = 3000, global = true)]
    no_speech_timeout_ms: u64,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, global = true)]
    hotkey_enabled: bool,

    #[arg(long, value_name = "KEY", global = true)]
    hotkey_key: Option<String>,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, global = true)]
    download_model: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    #[command(name = "transcript-path")]
    TranscriptPath,
}

#[derive(Subcommand, Debug, Copy, Clone, PartialEq, Eq)]
enum DaemonCommand {
    Start,
    Stop,
    #[command(name = "set-model")]
    SetModel {
        #[arg(long, value_name = "SIZE")]
        size: ModelSize,
        #[arg(long, value_name = "LANG")]
        model_language: ModelLanguage,
    },
    #[command(name = "test-audio")]
    TestAudio,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum CliMode {
    RunDaemon,
    StopDaemon,
    ShowTranscriptPath,
    SetModel {
        size: ModelSize,
        model_language: ModelLanguage,
    },
    ListDevices,
    TestAudio,
}

fn resolve_cli_mode(cli: &Cli) -> CliMode {
    match cli.command {
        Some(CliCommand::Daemon {
            command: DaemonCommand::Start,
        }) => CliMode::RunDaemon,
        Some(CliCommand::Daemon {
            command: DaemonCommand::Stop,
        }) => CliMode::StopDaemon,
        Some(CliCommand::TranscriptPath) => CliMode::ShowTranscriptPath,
        Some(CliCommand::Daemon {
            command:
                DaemonCommand::SetModel {
                    size,
                    model_language,
                },
        }) => CliMode::SetModel {
            size,
            model_language,
        },
        Some(CliCommand::Daemon {
            command: DaemonCommand::TestAudio,
        }) => CliMode::TestAudio,
        None => {
            if cli.list_devices {
                CliMode::ListDevices
            } else {
                CliMode::RunDaemon
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Config {
    model_path: Option<PathBuf>,
    model_size: ModelSize,
    model_language: ModelLanguage,
    download_model: bool,
    language: String,
    device: Option<String>,
    audio_host: AudioHost,
    sample_rate: u32,
    format: OutputFormat,
    mode: OutputMode,
    output: OutputConfig,
    vad: VadMode,
    vad_silence_ms: u64,
    vad_threshold: f32,
    vad_chunk_ms: u64,
    segment_target_ms: u64,
    segment_grace_ms: u64,
    segment_overlap_ms: u64,
    segment_min_ms: u64,
    debug_audio: bool,
    debug_vad: bool,
    list_devices: bool,
    dump_audio: bool,
    audio_feedback: bool,
    no_speech_timeout_ms: u64,
    hotkey: HotkeyConfig,
}

struct ConfigSources<'a> {
    matches: &'a clap::ArgMatches,
}

impl ConfigSources<'_> {
    fn value<T>(&self, name: &str, cli: T, file: Option<T>) -> T {
        if self.matches.value_source(name) == Some(ValueSource::CommandLine) {
            cli
        } else {
            file.unwrap_or(cli)
        }
    }

    fn optional<T>(&self, name: &str, cli: Option<T>, file: Option<T>) -> Option<T> {
        if self.matches.value_source(name) == Some(ValueSource::CommandLine) {
            cli
        } else {
            cli.or(file)
        }
    }
}

impl Config {
    fn from_sources(cli: Cli, matches: &clap::ArgMatches, file: FileConfig) -> Self {
        let sources = ConfigSources { matches };
        let language = sources.value("language", cli.language, file.language);
        let model_size = sources.value("model_size", cli.model_size, file.model_size);

        let (model_language, model_language_explicit) =
            if matches.value_source("model_language") == Some(ValueSource::CommandLine) {
                (cli.model_language, true)
            } else if let Some(model_language) = file.model_language {
                (model_language, true)
            } else {
                (cli.model_language, false)
            };
        let model_language = if model_size == ModelSize::LargeV3Turbo && !model_language_explicit {
            ModelLanguage::Auto
        } else if model_language_explicit {
            model_language
        } else {
            model_language_for_transcription(&language)
        };

        let device = sources.optional("device", cli.device, file.device);
        let audio_host = sources
            .optional("audio_host", cli.audio_host, file.audio_host)
            .unwrap_or_else(AudioHost::default_for_platform);
        let sample_rate = sources.value("sample_rate", cli.sample_rate, file.sample_rate);
        let format = sources.value("format", cli.format, file.format);

        let output_file = file.output.unwrap_or_default();
        let mode = sources.value("mode", cli.mode, output_file.mode);
        let output = OutputConfig {
            paste_keys: sources.value("paste_keys", cli.paste_keys, output_file.paste_keys),
            restore_clipboard: sources.value(
                "restore_clipboard",
                cli.restore_clipboard,
                output_file.restore_clipboard,
            ),
            pre_paste_delay_ms: sources.value(
                "pre_paste_delay_ms",
                cli.pre_paste_delay_ms,
                output_file.pre_paste_delay_ms,
            ),
            restore_clipboard_delay_ms: sources.value(
                "restore_clipboard_delay_ms",
                cli.restore_clipboard_delay_ms,
                output_file.restore_clipboard_delay_ms,
            ),
        };

        let vad = sources.value("vad", cli.vad, file.vad.map(VadSetting::into_mode));
        let vad_silence_ms =
            sources.value("vad_silence_ms", cli.vad_silence_ms, file.vad_silence_ms);
        let vad_threshold = sources.value("vad_threshold", cli.vad_threshold, file.vad_threshold);
        let vad_chunk_ms = sources.value("vad_chunk_ms", cli.vad_chunk_ms, file.vad_chunk_ms);
        let segment_target_ms = sources.value(
            "segment_target_ms",
            cli.segment_target_ms,
            file.segment_target_ms,
        );
        let segment_grace_ms = sources.value(
            "segment_grace_ms",
            cli.segment_grace_ms,
            file.segment_grace_ms,
        );
        let segment_overlap_ms = sources.value(
            "segment_overlap_ms",
            cli.segment_overlap_ms,
            file.segment_overlap_ms,
        );
        let segment_min_ms =
            sources.value("segment_min_ms", cli.segment_min_ms, file.segment_min_ms);
        let debug_audio = sources.value("debug_audio", cli.debug_audio, file.debug_audio);
        let debug_vad = sources.value("debug_vad", cli.debug_vad, file.debug_vad);
        let list_devices = sources.value("list_devices", cli.list_devices, file.list_devices);
        let dump_audio = sources.value("dump_audio", cli.dump_audio, file.dump_audio);
        let audio_feedback =
            sources.value("audio_feedback", cli.audio_feedback, file.audio_feedback);
        let no_speech_timeout_ms = sources.value(
            "no_speech_timeout_ms",
            cli.no_speech_timeout_ms,
            file.no_speech_timeout_ms,
        );

        let hotkey_file = file.hotkey.unwrap_or_default();
        let hotkey = HotkeyConfig {
            enabled: sources.value(
                "hotkey_enabled",
                cli.hotkey_enabled,
                Some(hotkey_file.enabled),
            ),
            key: sources.optional("hotkey_key", cli.hotkey_key, hotkey_file.key),
        };

        let download_model =
            sources.value("download_model", cli.download_model, file.download_model);
        let file_model_path = file.model_path.or(file.model);
        let model_path = sources.optional("model", cli.model, file_model_path);

        Self {
            model_path,
            model_size,
            model_language,
            download_model,
            language,
            device,
            audio_host,
            sample_rate,
            format,
            mode,
            output,
            vad,
            vad_silence_ms,
            vad_threshold,
            vad_chunk_ms,
            segment_target_ms,
            segment_grace_ms,
            segment_overlap_ms,
            segment_min_ms,
            debug_audio,
            debug_vad,
            list_devices,
            dump_audio,
            audio_feedback,
            no_speech_timeout_ms,
            hotkey,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    model: Option<PathBuf>,
    model_path: Option<PathBuf>,
    model_size: Option<ModelSize>,
    model_language: Option<ModelLanguage>,
    download_model: Option<bool>,
    language: Option<String>,
    device: Option<String>,
    audio_host: Option<AudioHost>,
    sample_rate: Option<u32>,
    format: Option<OutputFormat>,
    output: Option<FileOutputConfig>,
    vad: Option<VadSetting>,
    vad_silence_ms: Option<u64>,
    vad_threshold: Option<f32>,
    vad_chunk_ms: Option<u64>,
    segment_target_ms: Option<u64>,
    segment_grace_ms: Option<u64>,
    segment_overlap_ms: Option<u64>,
    segment_min_ms: Option<u64>,
    debug_audio: Option<bool>,
    debug_vad: Option<bool>,
    list_devices: Option<bool>,
    dump_audio: Option<bool>,
    audio_feedback: Option<bool>,
    no_speech_timeout_ms: Option<u64>,
    hotkey: Option<HotkeyConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileOutputConfig {
    mode: Option<OutputMode>,
    paste_keys: Option<String>,
    restore_clipboard: Option<bool>,
    pre_paste_delay_ms: Option<u64>,
    restore_clipboard_delay_ms: Option<u64>,
}

fn main() {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("Failed to parse CLI arguments");
    let mode = resolve_cli_mode(&cli);
    match mode {
        CliMode::StopDaemon => {
            if let Err(err) = daemon::send_stop_command() {
                eprintln!("error: {err}");
                process::exit(err.exit_code());
            }
            return;
        }
        CliMode::ShowTranscriptPath => {
            println!("{}", daemon::transcript_file_path().display());
            return;
        }
        CliMode::SetModel {
            size,
            model_language,
        } => {
            if let Err(err) = daemon::send_set_model_command(size, model_language) {
                eprintln!("error: {err}");
                process::exit(err.exit_code());
            }
            return;
        }
        CliMode::RunDaemon | CliMode::ListDevices | CliMode::TestAudio => {}
    }
    let file_config = match load_config_file() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(err.exit_code());
        }
    };
    let mut config = Config::from_sources(cli, &matches, file_config);
    if mode == CliMode::RunDaemon {
        config.list_devices = false;
    }

    let prepared_model = if config.list_devices {
        None
    } else {
        let spec = ModelSpec::new(config.model_size, config.model_language);
        match sv::model::prepare_model(config.model_path.as_deref(), &spec, config.download_model) {
            Ok(prepared) => Some(prepared),
            Err(err) => {
                eprintln!("error: {err}");
                process::exit(err.exit_code());
            }
        }
    };

    println!("SoundVibes sv {}", env!("CARGO_PKG_VERSION"));
    if let Some(prepared) = &prepared_model {
        if prepared.downloaded {
            println!("Model download complete.");
        }
        println!("Model: {}", prepared.path.display());
    }
    println!("Language: {}", config.language);
    println!("Sample rate: {} Hz", config.sample_rate);
    println!("Format: {:?}", config.format);
    println!("Mode: {:?}", config.mode);
    println!("VAD: {:?}", config.vad);
    println!("VAD silence timeout: {} ms", config.vad_silence_ms);
    println!("VAD threshold: {:.4}", config.vad_threshold);
    println!("VAD chunk: {} ms", config.vad_chunk_ms);
    println!("Segment target: {} ms", config.segment_target_ms);
    println!("Segment grace: {} ms", config.segment_grace_ms);
    println!("Segment overlap: {} ms", config.segment_overlap_ms);
    println!("Segment minimum: {} ms", config.segment_min_ms);
    println!("Dump audio: {}", config.dump_audio);
    println!("Audio host: {:?}", config.audio_host);
    if let Some(device) = &config.device {
        println!("Device: {device}");
    }

    let result = if config.list_devices {
        run_list_devices(&config)
    } else if mode == CliMode::TestAudio {
        run_test_audio(&config)
    } else {
        let model_path = prepared_model
            .as_ref()
            .map(|prepared| prepared.path.clone());
        let daemon_config = daemon::DaemonConfig {
            model_path,
            download_model: config.download_model,
            language: config.language.clone(),
            device: config.device.clone(),
            audio_host: config.audio_host,
            sample_rate: config.sample_rate,
            format: config.format,
            mode: config.mode,
            output: config.output.clone(),
            vad: config.vad,
            vad_silence_ms: config.vad_silence_ms,
            vad_threshold: config.vad_threshold,
            vad_chunk_ms: config.vad_chunk_ms,
            segment_target_ms: config.segment_target_ms,
            segment_grace_ms: config.segment_grace_ms,
            segment_overlap_ms: config.segment_overlap_ms,
            segment_min_ms: config.segment_min_ms,
            debug_audio: config.debug_audio,
            debug_vad: config.debug_vad,
            dump_audio: config.dump_audio,
            audio_feedback: config.audio_feedback,
            no_speech_timeout_ms: config.no_speech_timeout_ms,
            hotkey: config.hotkey.clone(),
        };
        let deps = daemon::DaemonDeps::default();
        let mut output = daemon::StdoutOutput;
        daemon::run_daemon(&daemon_config, &deps, &mut output)
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        process::exit(err.exit_code());
    }
}

fn load_config_file() -> Result<FileConfig, AppError> {
    let path = match config_path() {
        Some(path) => path,
        None => return Ok(FileConfig::default()),
    };

    if !path.exists() {
        return Ok(FileConfig::default());
    }

    let contents = fs::read_to_string(&path).map_err(|err| {
        AppError::config(format!(
            "failed to read config file {}: {err}",
            path.display()
        ))
    })?;
    toml::from_str(&contents).map_err(|err| {
        AppError::config(format!(
            "failed to parse config file {}: {err}",
            path.display()
        ))
    })
}

fn config_path() -> Option<PathBuf> {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_home.join("soundvibes").join("config.toml"))
}

fn run_list_devices(config: &Config) -> Result<(), AppError> {
    let host = daemon::select_audio_host(config.audio_host)?;
    audio::configure_alsa_logging(config.debug_audio);
    let devices = audio::list_input_devices(&host).map_err(|err| AppError::audio(err.message))?;
    println!("Input devices:");
    for name in devices {
        println!("  - {name}");
    }
    Ok(())
}

fn run_test_audio(config: &Config) -> Result<(), AppError> {
    use std::io::Write;

    let host = daemon::select_audio_host(config.audio_host)?;
    audio::configure_alsa_logging(config.debug_audio);

    let mut capture = audio::start_capture(&host, config.device.as_deref(), config.sample_rate)
        .map_err(|err| AppError::audio(err.message))?;

    let confirm_samples = (0.1 * config.sample_rate as f32) as usize; // 100ms
    let mut speech_detector =
        audio::SpeechDetector::new(config.vad_threshold, 100, config.sample_rate);

    println!(
        "Testing audio levels. Threshold: {:.4}",
        config.vad_threshold
    );
    println!("Speak to see if speech is detected. Press Ctrl+C to stop.\n");
    println!(
        "{:>10} {:>10} {:>12} {:>8}",
        "RMS", "Threshold", "Accumulated", "Status"
    );
    println!("{:-<10} {:-<10} {:-<12} {:-<8}", "", "", "", "");

    let mut buffer = Vec::new();

    loop {
        audio::drain_samples(&mut capture, &mut buffer);
        if buffer.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }

        let rms = audio::rms_energy(&buffer);
        let above_threshold = rms >= config.vad_threshold;
        if !above_threshold {
            speech_detector.reset();
        }
        let detected = speech_detector.process(&buffer);
        let status = if detected {
            "DETECTED"
        } else if above_threshold {
            "above"
        } else {
            "silent"
        };

        print!(
            "\r{:>10.6} {:>10.4} {:>12} {:>8}",
            rms,
            config.vad_threshold,
            format!("{}/{}", speech_detector.speech_samples(), confirm_samples),
            status
        );
        std::io::stdout().flush().ok();

        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
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

    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("test lock poisoned")
    }

    fn temp_runtime_dir() -> PathBuf {
        let mut dir = env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        dir.push(format!("soundvibes-test-{}-{stamp}", process::id()));
        dir
    }

    #[test]
    fn record_start_command_reaches_daemon_socket() -> Result<(), AppError> {
        let _lock = lock_tests();
        let runtime_dir = temp_runtime_dir();
        fs::create_dir_all(&runtime_dir).map_err(|err| {
            AppError::runtime(format!("failed to create test runtime dir: {err}"))
        })?;
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &runtime_dir);
        let socket_path = daemon::daemon_socket_path()?;
        let (_socket_guard, control_events, _sender) = daemon::start_socket_listener(&socket_path)?;

        daemon::send_record_start_command()?;

        match control_events.recv_timeout(Duration::from_secs(1)) {
            Ok(daemon::ControlEvent::StartRecording) => Ok(()),
            Ok(daemon::ControlEvent::StopRecording) => {
                Err(AppError::runtime("unexpected record-stop event"))
            }
            Ok(daemon::ControlEvent::Stop) => Err(AppError::runtime("unexpected stop event")),
            Ok(daemon::ControlEvent::SetModel { .. }) => {
                Err(AppError::runtime("unexpected set-model event"))
            }
            Ok(daemon::ControlEvent::Error(message)) => Err(AppError::runtime(message)),
            Err(_) => Err(AppError::runtime("record-start command not received")),
        }
    }

    #[test]
    fn record_start_command_errors_when_socket_missing() {
        let _lock = lock_tests();
        let runtime_dir = temp_runtime_dir();
        fs::create_dir_all(&runtime_dir).expect("failed to create test runtime dir");
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &runtime_dir);

        let err = daemon::send_record_start_command().expect_err("expected socket error");
        assert!(err.to_string().contains("daemon socket not found"));
    }

    #[test]
    fn record_start_command_errors_when_socket_unavailable() {
        let _lock = lock_tests();
        let runtime_dir = temp_runtime_dir();
        fs::create_dir_all(&runtime_dir).expect("failed to create test runtime dir");
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &runtime_dir);
        let socket_path = daemon::daemon_socket_path().expect("failed to compute socket path");
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).expect("failed to create socket dir");
        }
        fs::write(&socket_path, b"not-a-socket").expect("failed to create socket file");

        let err = daemon::send_record_start_command().expect_err("expected socket error");
        assert!(err.to_string().contains("daemon socket unavailable"));
        assert!(err.to_string().contains("sv daemon start"));
    }

    #[test]
    fn defaults_to_daemon_when_no_subcommand() {
        let cli = Cli::try_parse_from(["sv"]).expect("failed to parse cli");
        assert_eq!(resolve_cli_mode(&cli), CliMode::RunDaemon);
    }

    #[test]
    fn parses_daemon_start_subcommand() {
        let cli = Cli::try_parse_from(["sv", "daemon", "start"]).expect("failed to parse cli");
        assert_eq!(resolve_cli_mode(&cli), CliMode::RunDaemon);
    }

    #[test]
    fn parses_daemon_stop_subcommand() {
        let cli = Cli::try_parse_from(["sv", "daemon", "stop"]).expect("failed to parse cli");
        assert_eq!(resolve_cli_mode(&cli), CliMode::StopDaemon);
    }

    #[test]
    fn parses_daemon_set_model_subcommand() {
        let cli = Cli::try_parse_from([
            "sv",
            "daemon",
            "set-model",
            "--size",
            "small",
            "--model-language",
            "en",
        ])
        .expect("failed to parse cli");
        assert_eq!(
            resolve_cli_mode(&cli),
            CliMode::SetModel {
                size: ModelSize::Small,
                model_language: ModelLanguage::En,
            }
        );
    }

    #[test]
    fn parses_large_v3_turbo_model_size() {
        let cli = Cli::try_parse_from([
            "sv",
            "daemon",
            "set-model",
            "--size",
            "large-v3-turbo",
            "--model-language",
            "auto",
        ])
        .expect("failed to parse cli");
        assert_eq!(
            resolve_cli_mode(&cli),
            CliMode::SetModel {
                size: ModelSize::LargeV3Turbo,
                model_language: ModelLanguage::Auto,
            }
        );
    }

    #[test]
    fn large_v3_turbo_defaults_to_multilingual_model() {
        let command = Cli::command();
        let matches = command
            .try_get_matches_from(["sv", "daemon", "start", "--model-size", "large-v3-turbo"])
            .expect("failed to parse cli");
        let cli = Cli::from_arg_matches(&matches).expect("failed to build cli");
        let config = Config::from_sources(cli, &matches, FileConfig::default());

        assert_eq!(config.model_size, ModelSize::LargeV3Turbo);
        assert_eq!(config.model_language, ModelLanguage::Auto);
    }

    #[test]
    fn parses_transcript_path_subcommand() {
        let cli = Cli::try_parse_from(["sv", "transcript-path"]).expect("failed to parse cli");
        assert_eq!(resolve_cli_mode(&cli), CliMode::ShowTranscriptPath);
    }

    #[test]
    fn defaults_to_paste_mode_with_clipboard_restore() {
        let command = Cli::command();
        let matches = command
            .try_get_matches_from(["sv", "daemon", "start"])
            .expect("failed to parse cli");
        let cli = Cli::from_arg_matches(&matches).expect("failed to build cli");
        let config = Config::from_sources(cli, &matches, FileConfig::default());

        assert_eq!(config.mode, OutputMode::Paste);
        assert!(config.output.restore_clipboard);
        assert_eq!(config.output.paste_keys, "ctrl+v");
        assert_eq!(config.output.pre_paste_delay_ms, 100);
        assert_eq!(config.output.restore_clipboard_delay_ms, 250);
        assert!(config.hotkey.enabled);
        assert_eq!(config.hotkey.key, None);
    }

    #[test]
    fn reads_hotkey_config_from_hotkey_table() {
        let file: FileConfig = toml::from_str(
            r#"
            [hotkey]
            enabled = true
            key = "RIGHTCTRL"
            "#,
        )
        .expect("config should parse");
        let command = Cli::command();
        let matches = command
            .try_get_matches_from(["sv", "daemon", "start"])
            .expect("failed to parse cli");
        let cli = Cli::from_arg_matches(&matches).expect("failed to build cli");
        let config = Config::from_sources(cli, &matches, file);

        assert!(config.hotkey.enabled);
        assert_eq!(config.hotkey.key.as_deref(), Some("RIGHTCTRL"));
    }

    #[test]
    fn reads_paste_output_config_from_output_table() {
        let file: FileConfig = toml::from_str(
            r#"
            [output]
            mode = "clipboard"
            paste_keys = "ctrl+shift+v"
            restore_clipboard = false
            pre_paste_delay_ms = 150
            restore_clipboard_delay_ms = 400
            "#,
        )
        .expect("config should parse");
        let command = Cli::command();
        let matches = command
            .try_get_matches_from(["sv", "daemon", "start"])
            .expect("failed to parse cli");
        let cli = Cli::from_arg_matches(&matches).expect("failed to build cli");
        let config = Config::from_sources(cli, &matches, file);

        assert_eq!(config.mode, OutputMode::Clipboard);
        assert_eq!(config.output.paste_keys, "ctrl+shift+v");
        assert!(!config.output.restore_clipboard);
        assert_eq!(config.output.pre_paste_delay_ms, 150);
        assert_eq!(config.output.restore_clipboard_delay_ms, 400);
    }

    #[test]
    fn reads_segment_config_with_cli_overrides() {
        let file: FileConfig = toml::from_str(
            r#"
            segment_target_ms = 9000
            segment_grace_ms = 1500
            segment_overlap_ms = 300
            segment_min_ms = 900
            "#,
        )
        .expect("config should parse");
        let command = Cli::command();
        let matches = command
            .try_get_matches_from([
                "sv",
                "daemon",
                "start",
                "--segment-target-ms",
                "7000",
                "--segment-overlap-ms",
                "250",
            ])
            .expect("failed to parse cli");
        let cli = Cli::from_arg_matches(&matches).expect("failed to build cli");
        let config = Config::from_sources(cli, &matches, file);

        assert_eq!(config.segment_target_ms, 7000);
        assert_eq!(config.segment_grace_ms, 1500);
        assert_eq!(config.segment_overlap_ms, 250);
        assert_eq!(config.segment_min_ms, 900);
    }
}
