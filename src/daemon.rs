use chrono::{Local, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::flag;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::audio;
use crate::error::AppError;
use crate::feedback;
use crate::hotkey::{self, HotkeyConfig};
use crate::model::{self, ModelLanguage, ModelSize, ModelSpec};
use crate::output::{self, OutputConfig, OutputMode};
use crate::segmentation::{self, CutReason, SegmentConfig, SegmentDecision};
pub use crate::transcription_worker::Transcriber;
use crate::transcription_worker::{TranscriptionJob, TranscriptionResult, TranscriptionWorker};
use crate::types::{AudioHost, OutputFormat, VadMode};
use crate::whisper::WhisperContext;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub model_path: Option<PathBuf>,
    pub download_model: bool,
    pub language: String,
    pub device: Option<String>,
    pub audio_host: AudioHost,
    pub sample_rate: u32,
    pub format: OutputFormat,
    pub output: OutputConfig,
    pub vad: VadMode,
    pub vad_silence_ms: u64,
    pub vad_threshold: f32,
    pub vad_chunk_ms: u64,
    pub segment_target_ms: u64,
    pub segment_grace_ms: u64,
    pub segment_overlap_ms: u64,
    pub segment_min_ms: u64,
    pub debug_audio: bool,
    pub dump_audio: bool,
    pub audio_feedback: bool,
    pub no_speech_timeout_ms: u64,
    pub hotkey: HotkeyConfig,
}

pub trait DaemonOutput {
    fn stdout(&mut self, message: &str);
    fn stderr(&mut self, message: &str);
}

pub struct StdoutOutput;

impl DaemonOutput for StdoutOutput {
    fn stdout(&mut self, message: &str) {
        println!("{message}");
    }

    fn stderr(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

pub trait CaptureSource {
    fn drain(&mut self, output: &mut Vec<f32>);
}

pub trait AudioBackend {
    fn list_input_devices(&self, host: &cpal::Host) -> Result<Vec<String>, audio::AudioError>;
    fn start_capture(
        &self,
        host: &cpal::Host,
        device_name: Option<&str>,
        sample_rate: u32,
    ) -> Result<Box<dyn CaptureSource>, audio::AudioError>;
}

pub trait TranscriberFactory {
    fn load(&self, model_path: Option<&Path>) -> Result<Box<dyn Transcriber>, AppError>;
}

pub struct DaemonDeps {
    pub audio: Box<dyn AudioBackend>,
    pub transcriber_factory: Box<dyn TranscriberFactory>,
}

impl Default for DaemonDeps {
    fn default() -> Self {
        Self {
            audio: Box::new(CpalAudioBackend),
            transcriber_factory: Box::new(WhisperFactory),
        }
    }
}

pub fn select_audio_host(audio_host: AudioHost) -> Result<cpal::Host, AppError> {
    match audio_host {
        AudioHost::Default => Ok(cpal::default_host()),
        AudioHost::Alsa => {
            let host_id = cpal::HostId::Alsa;
            if !cpal::available_hosts().contains(&host_id) {
                let available = cpal::available_hosts()
                    .into_iter()
                    .map(|host| format!("{host:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(AppError::config(format!(
                    "audio host {audio_host:?} not available (available: {available})"
                )));
            }
            cpal::host_from_id(host_id)
                .map_err(|err| AppError::runtime(format!("failed to init audio host: {err}")))
        }
    }
}

const CONTROL_API_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub api_version: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ControlResponse {
    fn success(state: Option<&str>, language: Option<&str>) -> Self {
        Self {
            api_version: CONTROL_API_VERSION.to_string(),
            ok: true,
            state: state.map(str::to_string),
            language: language.map(str::to_string),
            message: None,
        }
    }

    fn success_with_message(state: &str, language: &str, message: String) -> Self {
        let mut response = Self::success(Some(state), Some(language));
        response.message = Some(message);
        response
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            api_version: CONTROL_API_VERSION.to_string(),
            ok: false,
            state: None,
            language: None,
            message: Some(message.into()),
        }
    }
}

#[derive(Debug)]
pub enum ControlEvent {
    StartRecording,
    StopRecording,
    Stop,
    Status,
    SetModel {
        size: ModelSize,
        model_language: ModelLanguage,
    },
    Error(String),
    Request {
        event: Box<ControlEvent>,
        respond_to: SyncSender<ControlResponse>,
    },
}

struct ActiveRecording {
    capture: Box<dyn CaptureSource>,
    buffer: Vec<f32>,
    has_leading_overlap: bool,
    trailing_silence_samples: usize,
    started: std::time::Instant,
    speech_detector: audio::SpeechDetector,
}

impl ActiveRecording {
    fn start(
        deps: &DaemonDeps,
        host: &cpal::Host,
        config: &DaemonConfig,
        output: &mut dyn DaemonOutput,
    ) -> Result<Self, AppError> {
        let capture = deps
            .audio
            .start_capture(host, config.device.as_deref(), config.sample_rate)
            .map_err(|err| AppError::audio(err.message))?;
        output.stdout("Recording started.");
        if config.audio_feedback {
            feedback::play_start_sound();
        }
        Ok(Self {
            capture,
            buffer: Vec::new(),
            has_leading_overlap: false,
            trailing_silence_samples: 0,
            started: std::time::Instant::now(),
            speech_detector: audio::SpeechDetector::new(
                config.vad_threshold,
                100,
                config.sample_rate,
            ),
        })
    }

    fn finish(
        mut self,
        worker: &mut TranscriptionWorker,
        config: &DaemonConfig,
        vad: &audio::VadConfig,
        output: &mut dyn DaemonOutput,
    ) -> Result<(), AppError> {
        self.capture.drain(&mut self.buffer);
        submit_final_recording(
            worker,
            config,
            vad,
            &self.buffer,
            self.has_leading_overlap,
            output,
        )
    }
}

pub fn run_daemon(
    config: &DaemonConfig,
    deps: &DaemonDeps,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    let socket_path = daemon_socket_path()?;
    let (_guard, control_events, control_sender) = start_socket_listener(&socket_path)?;
    output.stdout(&format!("Daemon listening on {}", socket_path.display()));

    let _hotkey_listener = if config.hotkey.enabled {
        Some(hotkey::start_listener(&config.hotkey, control_sender)?)
    } else {
        None
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM] {
        flag::register(signal, Arc::clone(&shutdown)).map_err(|err| {
            AppError::runtime(format!("failed to register signal handler: {err}"))
        })?;
    }

    run_daemon_loop(config, deps, output, control_events, &shutdown)
}

pub fn run_daemon_loop(
    config: &DaemonConfig,
    deps: &DaemonDeps,
    output: &mut dyn DaemonOutput,
    control_events: Receiver<ControlEvent>,
    shutdown: &AtomicBool,
) -> Result<(), AppError> {
    let host = select_audio_host(config.audio_host)?;
    audio::configure_alsa_logging(config.debug_audio);
    let devices = deps
        .audio
        .list_input_devices(&host)
        .map_err(|err| AppError::audio(err.message))?;
    output.stdout("Input devices:");
    for name in &devices {
        output.stdout(&format!("  - {name}"));
    }

    if let Some(device) = config.device.as_deref() {
        if !devices.iter().any(|name| name.eq_ignore_ascii_case(device)) {
            return Err(AppError::audio(format!("input device not found: {device}")));
        }
    }

    let transcriber = deps
        .transcriber_factory
        .load(config.model_path.as_deref())?;
    let mut worker = TranscriptionWorker::start(transcriber);

    let vad = audio::VadConfig::new(
        config.vad == VadMode::On || config.vad == VadMode::Continuous,
        config.vad_silence_ms,
        config.vad_threshold,
        config.vad_chunk_ms,
    );

    let mut recording: Option<ActiveRecording> = None;
    let mut last_emitted_transcript = String::new();
    // Grace period after recording starts to ignore audio feedback pickup (500ms)
    let speech_detection_grace_ms: u64 = if config.audio_feedback { 500 } else { 0 };
    let segment_config = segment_config(config);

    loop {
        drain_worker_results(&mut worker, config, output, &mut last_emitted_transcript);

        if shutdown.load(Ordering::Relaxed) {
            if let Some(active) = recording.take() {
                active.finish(&mut worker, config, &vad, output)?;
            }
            wait_for_pending_results(&mut worker, config, output, &mut last_emitted_transcript);
            worker.shutdown()?;
            output.stdout("Daemon shutting down.");
            break;
        }
        let received = match control_events.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(AppError::runtime("socket listener disconnected"))
            }
        };
        if let Some(received) = received {
            let (event, respond_to) = match received {
                ControlEvent::Request { event, respond_to } => (*event, Some(respond_to)),
                event => (event, None),
            };
            match event {
                ControlEvent::StartRecording => {
                    if recording.is_none() {
                        match ActiveRecording::start(deps, &host, config, output) {
                            Ok(active) => recording = Some(active),
                            Err(err) if respond_to.is_some() => {
                                acknowledge_error(respond_to.as_ref(), &err);
                                continue;
                            }
                            Err(err) => return Err(err),
                        }
                    }
                    acknowledge_success(respond_to.as_ref(), &recording, config, None);
                }
                ControlEvent::StopRecording => {
                    if let Some(active) = recording.take() {
                        if let Err(err) = active.finish(&mut worker, config, &vad, output) {
                            if respond_to.is_some() {
                                acknowledge_error(respond_to.as_ref(), &err);
                                continue;
                            }
                            return Err(err);
                        }
                        wait_for_pending_results(
                            &mut worker,
                            config,
                            output,
                            &mut last_emitted_transcript,
                        );
                        output.stdout("Ready for next utterance.");
                        if config.audio_feedback {
                            feedback::play_stop_sound();
                        }
                    }
                    acknowledge_success(respond_to.as_ref(), &recording, config, None);
                }
                ControlEvent::Stop => {
                    acknowledge_success(respond_to.as_ref(), &recording, config, None);
                    shutdown.store(true, Ordering::Relaxed);
                }
                ControlEvent::Status => {
                    acknowledge_success(respond_to.as_ref(), &recording, config, None);
                }
                ControlEvent::SetModel {
                    size,
                    model_language,
                } => {
                    match reload_model(
                        size,
                        model_language,
                        &mut recording,
                        &mut worker,
                        config,
                        deps,
                        output,
                        &mut last_emitted_transcript,
                    ) {
                        Ok(message) => {
                            output.stdout(&message);
                            acknowledge_success(
                                respond_to.as_ref(),
                                &recording,
                                config,
                                Some(message),
                            );
                        }
                        Err(err) => {
                            output.stderr(&format!("Model reload failed: {err}"));
                            acknowledge_error(respond_to.as_ref(), &err);
                        }
                    }
                }
                ControlEvent::Error(message) => return Err(AppError::runtime(message)),
                ControlEvent::Request { .. } => {
                    unreachable!("control request was already unwrapped")
                }
            }
        }

        if let Some(active) = recording.as_mut() {
            let prev_len = active.buffer.len();
            active.capture.drain(&mut active.buffer);
            let new_samples = active.buffer.len() - prev_len;

            // Check for speech in new samples
            if new_samples > 0 {
                let new_audio = &active.buffer[prev_len..];
                let rms = audio::rms_energy(new_audio);

                // Track sustained speech for no-speech timeout (skip grace period for audio feedback)
                let in_grace_period =
                    (active.started.elapsed().as_millis() as u64) < speech_detection_grace_ms;

                if !in_grace_period {
                    active.speech_detector.process(new_audio);
                }

                if config.vad == VadMode::Continuous {
                    if rms < config.vad_threshold {
                        active.trailing_silence_samples += new_samples;
                    } else {
                        active.trailing_silence_samples = 0;
                    }

                    if let SegmentDecision::Cut { speech_end, reason } =
                        segmentation::decide_segment(
                            &segment_config,
                            active.buffer.len(),
                            active.trailing_silence_samples,
                            rms,
                        )
                    {
                        submit_segment(
                            &mut worker,
                            config,
                            &active.buffer[..speech_end],
                            active.has_leading_overlap,
                            output,
                        )?;
                        active.buffer = segmentation::carry_after_cut(
                            &active.buffer,
                            speech_end,
                            &segment_config,
                            reason,
                        );
                        active.has_leading_overlap = reason != CutReason::Silence
                            && carried_overlap_contains_speech(
                                &active.buffer,
                                segment_config.sample_rate,
                                segment_config.vad_threshold,
                                config.vad_chunk_ms,
                            );
                        active.trailing_silence_samples = 0;
                        active.speech_detector.reset();
                        active.started = std::time::Instant::now();
                    }
                }
            }

            // Check for no-speech timeout
            if config.no_speech_timeout_ms > 0
                && !active.speech_detector.is_detected()
                && (active.started.elapsed().as_millis() as u64) >= config.no_speech_timeout_ms
            {
                recording = None;
                output.stdout("No speech detected, cancelled.");
                if config.audio_feedback {
                    feedback::play_stop_sound();
                }
            }
        }
    }
    Ok(())
}

fn acknowledge_success(
    respond_to: Option<&SyncSender<ControlResponse>>,
    recording: &Option<ActiveRecording>,
    config: &DaemonConfig,
    message: Option<String>,
) {
    let Some(respond_to) = respond_to else {
        return;
    };
    let state = if recording.is_some() {
        "recording"
    } else {
        "idle"
    };
    let response = match message {
        Some(message) => ControlResponse::success_with_message(state, &config.language, message),
        None => ControlResponse::success(Some(state), Some(&config.language)),
    };
    let _ = respond_to.send(response);
}

fn acknowledge_error(respond_to: Option<&SyncSender<ControlResponse>>, err: &AppError) {
    if let Some(respond_to) = respond_to {
        let _ = respond_to.send(ControlResponse::error(err.to_string()));
    }
}

#[allow(clippy::too_many_arguments)]
fn reload_model(
    size: ModelSize,
    model_language: ModelLanguage,
    recording: &mut Option<ActiveRecording>,
    worker: &mut TranscriptionWorker,
    config: &DaemonConfig,
    deps: &DaemonDeps,
    output: &mut dyn DaemonOutput,
    last_emitted_transcript: &mut String,
) -> Result<String, AppError> {
    if recording.take().is_some() {
        output.stdout("Recording stopped for model reload.");
    }
    wait_for_pending_results(worker, config, output, last_emitted_transcript);
    let spec = ModelSpec::new(size, model_language);
    let prepared = model::prepare_model(None, &spec, config.download_model)?;
    if prepared.downloaded {
        output.stdout("Model download complete.");
    }
    let new_transcriber = deps.transcriber_factory.load(Some(&prepared.path))?;
    worker.reload(new_transcriber)?;
    Ok(format!(
        "Model reloaded: size={size}, model-language={model_language}"
    ))
}

fn submit_final_recording(
    worker: &mut TranscriptionWorker,
    config: &DaemonConfig,
    vad: &audio::VadConfig,
    buffer: &[f32],
    had_overlap: bool,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    let trimmed = audio::trim_trailing_silence(buffer, config.sample_rate, vad);
    if trimmed.is_empty() {
        return Ok(());
    }
    submit_segment(worker, config, &trimmed, had_overlap, output)
}

fn submit_segment(
    worker: &mut TranscriptionWorker,
    config: &DaemonConfig,
    samples: &[f32],
    had_overlap: bool,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    if samples.is_empty() {
        return Ok(());
    }

    if config.dump_audio {
        dump_audio_samples(samples, config.sample_rate, output)?;
    }
    worker.submit(TranscriptionJob {
        samples: samples.to_vec(),
        duration_ms: audio::samples_to_ms(samples.len(), config.sample_rate),
        language: Some(config.language.clone()),
        had_overlap,
    })
}

fn drain_worker_results(
    worker: &mut TranscriptionWorker,
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    last_emitted_transcript: &mut String,
) {
    while let Some(result) = worker.try_recv() {
        emit_worker_result(config, output, result, last_emitted_transcript);
    }
}

fn wait_for_pending_results(
    worker: &mut TranscriptionWorker,
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    last_emitted_transcript: &mut String,
) {
    while worker.has_pending() {
        match worker.recv() {
            Some(result) => emit_worker_result(config, output, result, last_emitted_transcript),
            None => break,
        }
    }
}

fn emit_worker_result(
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    result: TranscriptionResult,
    last_emitted_transcript: &mut String,
) {
    match result.transcript {
        Ok(transcript) => {
            let text = if result.had_overlap && !last_emitted_transcript.trim().is_empty() {
                segmentation::dedupe_boundary(last_emitted_transcript, &transcript)
            } else {
                transcript
            };
            if !text.trim().is_empty() {
                emit_transcript(
                    config,
                    output,
                    &text,
                    audio::SegmentInfo {
                        index: result.index,
                        duration_ms: result.duration_ms,
                    },
                );
                *last_emitted_transcript = text;
            }
        }
        Err(err) => output.stderr(&format!("Transcription error: {err}")),
    }
}

fn segment_config(config: &DaemonConfig) -> SegmentConfig {
    SegmentConfig {
        sample_rate: config.sample_rate,
        vad_threshold: config.vad_threshold,
        silence_samples: samples_from_ms(config.vad_silence_ms, config.sample_rate),
        target_samples: samples_from_ms(config.segment_target_ms, config.sample_rate),
        grace_samples: samples_from_ms(config.segment_grace_ms, config.sample_rate),
        overlap_samples: samples_from_ms(config.segment_overlap_ms, config.sample_rate),
        min_segment_samples: samples_from_ms(config.segment_min_ms, config.sample_rate),
    }
}

fn samples_from_ms(ms: u64, sample_rate: u32) -> usize {
    ((ms as f64 / 1000.0) * sample_rate as f64).round() as usize
}

fn carried_overlap_contains_speech(
    samples: &[f32],
    sample_rate: u32,
    vad_threshold: f32,
    vad_chunk_ms: u64,
) -> bool {
    if samples.is_empty() {
        return false;
    }
    let chunk_samples = samples_from_ms(vad_chunk_ms, sample_rate).max(1);
    samples
        .chunks(chunk_samples)
        .any(|chunk| audio::rms_energy(chunk) >= vad_threshold)
}

fn emit_transcript(
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    text: &str,
    info: audio::SegmentInfo,
) {
    match config.output.mode {
        OutputMode::Stdout => emit_stdout(config.format, output, text, info),
        OutputMode::Clipboard => {
            if let Err(err) = output::output_text(text, &config.output) {
                output.stderr(&format!("warn: {err}; falling back to stdout"));
                emit_stdout(config.format, output, text, info)
            }
        }
        OutputMode::Paste | OutputMode::Type => {
            let insertion_text = segmentation::append_segment_space(text);
            if let Err(err) = output::output_text(&insertion_text, &config.output) {
                output.stderr(&format!("warn: {err}; falling back to stdout"));
                emit_stdout(config.format, output, text, info)
            }
        }
    }
}

pub fn transcript_file_path() -> PathBuf {
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let date = Local::now().format("%Y-%m-%d").to_string();
    data_home
        .join("soundvibes")
        .join("transcripts")
        .join(format!("{date}.log"))
}

fn emit_stdout(
    format: OutputFormat,
    output: &mut dyn DaemonOutput,
    text: &str,
    info: audio::SegmentInfo,
) {
    match format {
        OutputFormat::Plain => {
            output.stdout(&format!("Transcript {}: {}", info.index, text));
        }
        OutputFormat::Jsonl => {
            output.stdout(
                &serde_json::json!({
                    "type": "final",
                    "utterance": info.index,
                    "duration_ms": info.duration_ms,
                    "timestamp": Utc::now().to_rfc3339(),
                    "text": text,
                })
                .to_string(),
            );
        }
    }
}

fn dump_audio_samples(
    samples: &[f32],
    sample_rate: u32,
    output: &mut dyn DaemonOutput,
) -> Result<PathBuf, AppError> {
    let output_dir = env::current_dir()
        .map_err(|err| AppError::runtime(format!("failed to read current dir: {err}")))?;
    let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("sv_{timestamp}.wav");
    let path = output_dir.join(filename);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec)
        .map_err(|err| AppError::runtime(format!("failed to create wav file: {err}")))?;
    for sample in samples {
        let clipped = sample.clamp(-1.0, 1.0);
        let value = (clipped * i16::MAX as f32) as i16;
        writer
            .write_sample(value)
            .map_err(|err| AppError::runtime(format!("failed to write wav data: {err}")))?;
    }
    writer
        .finalize()
        .map_err(|err| AppError::runtime(format!("failed to finalize wav: {err}")))?;
    output.stdout(&format!("Saved audio: {}", path.display()));
    Ok(path)
}

pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn daemon_socket_path() -> Result<PathBuf, AppError> {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        AppError::runtime(
            "XDG_RUNTIME_DIR is not set; set it to a writable runtime dir (e.g. /run/user/$(id -u))",
        )
    })?;
    Ok(PathBuf::from(runtime_dir)
        .join("soundvibes")
        .join("sv.sock"))
}

pub fn start_socket_listener(
    socket_path: &Path,
) -> Result<
    (
        SocketGuard,
        Receiver<ControlEvent>,
        mpsc::Sender<ControlEvent>,
    ),
    AppError,
> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::runtime(format!(
                "failed to create socket directory {}: {err}",
                parent.display()
            ))
        })?;
    }

    if socket_path.exists() {
        if UnixStream::connect(socket_path).is_ok() {
            return Err(AppError::runtime(
                "daemon already running; hold the configured hotkey to record",
            ));
        }
        fs::remove_file(socket_path).map_err(|err| {
            AppError::runtime(format!(
                "failed to remove stale daemon socket {}: {err}",
                socket_path.display()
            ))
        })?;
    }

    let listener = UnixListener::bind(socket_path).map_err(|err| {
        AppError::runtime(format!(
            "failed to bind daemon socket {}: {err}",
            socket_path.display()
        ))
    })?;
    let guard = SocketGuard {
        path: socket_path.to_path_buf(),
    };
    let (sender, receiver) = mpsc::channel();
    let socket_sender = sender.clone();

    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let mut buffer = String::new();
                    if let Err(err) = stream.read_to_string(&mut buffer) {
                        let _ = write_control_response(
                            &mut stream,
                            &ControlResponse::error(format!("socket read error: {err}")),
                        );
                        continue;
                    }
                    let command = buffer.trim();
                    let event = if command == "record-start" {
                        Ok(ControlEvent::StartRecording)
                    } else if command == "record-stop" {
                        Ok(ControlEvent::StopRecording)
                    } else if command == "stop" {
                        Ok(ControlEvent::Stop)
                    } else if command == "status" {
                        Ok(ControlEvent::Status)
                    } else if command.starts_with("set-model") {
                        match parse_set_model_command(command) {
                            Ok((size, model_language)) => Ok(ControlEvent::SetModel {
                                size,
                                model_language,
                            }),
                            Err(message) => Err(message),
                        }
                    } else {
                        Err(format!("unsupported daemon command: {command}"))
                    };

                    let event = match event {
                        Ok(event) => event,
                        Err(message) => {
                            let _ = write_control_response(
                                &mut stream,
                                &ControlResponse::error(message),
                            );
                            continue;
                        }
                    };

                    let (response_sender, response_receiver) = mpsc::sync_channel(1);
                    if socket_sender
                        .send(ControlEvent::Request {
                            event: Box::new(event),
                            respond_to: response_sender,
                        })
                        .is_err()
                    {
                        let _ = write_control_response(
                            &mut stream,
                            &ControlResponse::error("daemon loop is unavailable"),
                        );
                        break;
                    } else {
                        let response = response_receiver.recv().unwrap_or_else(|_| {
                            ControlResponse::error("daemon response channel closed")
                        });
                        let _ = write_control_response(&mut stream, &response);
                    }
                }
                Err(err) => {
                    let _ = socket_sender
                        .send(ControlEvent::Error(format!("socket listener error: {err}")));
                    break;
                }
            }
        }
    });

    Ok((guard, receiver, sender))
}

fn write_control_response(
    stream: &mut UnixStream,
    response: &ControlResponse,
) -> Result<(), AppError> {
    let mut line = serde_json::to_string(response)
        .map_err(|err| AppError::runtime(format!("failed to serialize daemon response: {err}")))?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|err| AppError::runtime(format!("failed to write daemon response: {err}")))
}

fn parse_set_model_command(command: &str) -> Result<(ModelSize, ModelLanguage), String> {
    let mut size = None;
    let mut model_language = None;
    for token in command.split_whitespace().skip(1) {
        if let Some(value) = token.strip_prefix("size=") {
            size = Some(ModelSize::from_str(value, true).map_err(|_| {
                format!("invalid model size '{value}' (expected auto|tiny|base|small|medium|large|large-v3-turbo)")
            })?);
        } else if let Some(value) = token.strip_prefix("model-language=") {
            model_language = Some(
                ModelLanguage::from_str(value, true)
                    .map_err(|_| format!("invalid model language '{value}' (expected auto|en)"))?,
            );
        }
    }

    let size = size.ok_or_else(|| "missing size=<SIZE>".to_string())?;
    let model_language =
        model_language.ok_or_else(|| "missing model-language=<LANG>".to_string())?;
    Ok((size, model_language))
}

pub fn send_record_start_command() -> Result<ControlResponse, AppError> {
    send_daemon_command("record-start")
}

pub fn send_record_stop_command() -> Result<ControlResponse, AppError> {
    send_daemon_command("record-stop")
}

pub fn send_stop_command() -> Result<ControlResponse, AppError> {
    send_daemon_command("stop")
}

pub fn send_status_command() -> Result<ControlResponse, AppError> {
    send_daemon_command("status")
}

pub fn send_set_model_command(
    size: ModelSize,
    model_language: ModelLanguage,
) -> Result<ControlResponse, AppError> {
    let command = format!("set-model size={size} model-language={model_language}");
    send_daemon_command(&command)
}

fn send_daemon_command(command: &str) -> Result<ControlResponse, AppError> {
    let socket_path = daemon_socket_path()?;
    if !socket_path.exists() {
        return Err(AppError::runtime(format!(
            "daemon socket not found at {}. Start it with `sv daemon start` or `systemctl --user start sv.service`",
            socket_path.display()
        )));
    }
    let mut stream = UnixStream::connect(&socket_path).map_err(|err| {
        AppError::runtime(format!(
            "daemon socket unavailable at {}. Start it with `sv daemon start` or `systemctl --user start sv.service` ({err})",
            socket_path.display()
        ))
    })?;
    let payload = format!("{command}\n");
    stream
        .write_all(payload.as_bytes())
        .map_err(|err| AppError::runtime(format!("failed to send {command}: {err}")))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|err| AppError::runtime(format!("failed to finalize {command}: {err}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(600)))
        .map_err(|err| AppError::runtime(format!("failed to configure daemon response: {err}")))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| AppError::runtime(format!("failed to read daemon response: {err}")))?;
    let response: ControlResponse = serde_json::from_str(response.trim()).map_err(|err| {
        AppError::runtime(format!(
            "failed to parse daemon response for {command}: {err}"
        ))
    })?;
    if response.ok {
        Ok(response)
    } else {
        Err(AppError::runtime(
            response
                .message
                .clone()
                .unwrap_or_else(|| "daemon command failed".to_string()),
        ))
    }
}

struct CpalAudioBackend;

impl AudioBackend for CpalAudioBackend {
    fn list_input_devices(&self, host: &cpal::Host) -> Result<Vec<String>, audio::AudioError> {
        audio::list_input_devices(host)
    }

    fn start_capture(
        &self,
        host: &cpal::Host,
        device_name: Option<&str>,
        sample_rate: u32,
    ) -> Result<Box<dyn CaptureSource>, audio::AudioError> {
        let capture = audio::start_capture(host, device_name, sample_rate)?;
        Ok(Box::new(CpalCapture { inner: capture }))
    }
}

struct CpalCapture {
    inner: audio::Capture,
}

impl CaptureSource for CpalCapture {
    fn drain(&mut self, output: &mut Vec<f32>) {
        audio::drain_samples(&mut self.inner, output);
    }
}

struct WhisperFactory;

impl TranscriberFactory for WhisperFactory {
    fn load(&self, model_path: Option<&Path>) -> Result<Box<dyn Transcriber>, AppError> {
        let model_path = model_path.ok_or_else(|| AppError::config("model path is required"))?;
        let context = WhisperContext::from_file(model_path)
            .map_err(|err| AppError::runtime(err.to_string()))?;
        Ok(Box::new(WhisperTranscriber { context }))
    }
}

struct WhisperTranscriber {
    context: WhisperContext,
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError> {
        self.context
            .transcribe(samples, language)
            .map_err(|err| AppError::runtime(err.to_string()))
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{
        AudioBackend, CaptureSource, DaemonConfig, DaemonOutput, Transcriber, TranscriberFactory,
    };
    use crate::audio::{AudioError, AudioErrorKind};
    use crate::error::AppError;
    use crate::hotkey::HotkeyConfig;
    use crate::output::{OutputConfig, OutputMode};
    use crate::segmentation::{
        DEFAULT_SEGMENT_GRACE_MS, DEFAULT_SEGMENT_MIN_MS, DEFAULT_SEGMENT_OVERLAP_MS,
        DEFAULT_SEGMENT_TARGET_MS,
    };
    use crate::types::{AudioHost, OutputFormat, VadMode};

    pub fn daemon_config() -> DaemonConfig {
        DaemonConfig {
            model_path: None,
            download_model: false,
            language: "en".to_string(),
            device: None,
            audio_host: AudioHost::Default,
            sample_rate: 16_000,
            format: OutputFormat::Plain,
            output: OutputConfig {
                mode: OutputMode::Stdout,
                ..OutputConfig::default()
            },
            vad: VadMode::Off,
            vad_silence_ms: 800,
            vad_threshold: 0.015,
            vad_chunk_ms: 250,
            segment_target_ms: DEFAULT_SEGMENT_TARGET_MS,
            segment_grace_ms: DEFAULT_SEGMENT_GRACE_MS,
            segment_overlap_ms: DEFAULT_SEGMENT_OVERLAP_MS,
            segment_min_ms: DEFAULT_SEGMENT_MIN_MS,
            debug_audio: false,
            dump_audio: false,
            audio_feedback: false,
            no_speech_timeout_ms: 0,
            hotkey: HotkeyConfig::default(),
        }
    }

    #[derive(Default)]
    pub struct TestOutput {
        stdout: Vec<String>,
        stderr: Vec<String>,
    }

    impl TestOutput {
        pub fn stdout_lines(&self) -> &[String] {
            &self.stdout
        }

        pub fn stderr_lines(&self) -> &[String] {
            &self.stderr
        }
    }

    impl DaemonOutput for TestOutput {
        fn stdout(&mut self, message: &str) {
            self.stdout.push(message.to_string());
        }

        fn stderr(&mut self, message: &str) {
            self.stderr.push(message.to_string());
        }
    }

    pub struct TestAudioBackend {
        devices: Vec<String>,
        chunks: Arc<Mutex<VecDeque<Vec<f32>>>>,
        start_error: Arc<Mutex<Option<AudioError>>>,
    }

    impl TestAudioBackend {
        pub fn new(devices: Vec<String>, chunks: Vec<Vec<f32>>) -> Self {
            Self {
                devices,
                chunks: Arc::new(Mutex::new(chunks.into())),
                start_error: Arc::new(Mutex::new(None)),
            }
        }

        pub fn with_start_error(devices: Vec<String>, error: AudioError) -> Self {
            Self {
                devices,
                chunks: Arc::new(Mutex::new(VecDeque::new())),
                start_error: Arc::new(Mutex::new(Some(error))),
            }
        }
    }

    impl AudioBackend for TestAudioBackend {
        fn list_input_devices(&self, _host: &cpal::Host) -> Result<Vec<String>, AudioError> {
            if self.devices.is_empty() {
                return Err(AudioError {
                    kind: AudioErrorKind::DeviceUnavailable,
                    message: "no input devices available".to_string(),
                });
            }
            Ok(self.devices.clone())
        }

        fn start_capture(
            &self,
            _host: &cpal::Host,
            device_name: Option<&str>,
            _sample_rate: u32,
        ) -> Result<Box<dyn CaptureSource>, AudioError> {
            if let Some(err) = self.start_error.lock().expect("audio error lock").take() {
                return Err(err);
            }
            if let Some(device) = device_name {
                if !self
                    .devices
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(device))
                {
                    return Err(AudioError {
                        kind: AudioErrorKind::DeviceNotFound,
                        message: format!("input device not found: {device}"),
                    });
                }
            }
            Ok(Box::new(TestCapture {
                chunks: Arc::clone(&self.chunks),
            }))
        }
    }

    struct TestCapture {
        chunks: Arc<Mutex<VecDeque<Vec<f32>>>>,
    }

    impl CaptureSource for TestCapture {
        fn drain(&mut self, output: &mut Vec<f32>) {
            if let Some(chunk) = self.chunks.lock().expect("audio chunk lock").pop_front() {
                output.extend(chunk);
            }
        }
    }

    #[derive(Clone)]
    pub struct TestTranscriberFactory {
        responses: Arc<Mutex<VecDeque<Result<String, AppError>>>>,
    }

    impl TestTranscriberFactory {
        pub fn new(responses: Vec<String>) -> Self {
            let responses = responses.into_iter().map(Ok).collect();
            Self {
                responses: Arc::new(Mutex::new(responses)),
            }
        }

        pub fn with_results(responses: Vec<Result<String, AppError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    impl TranscriberFactory for TestTranscriberFactory {
        fn load(&self, _model_path: Option<&Path>) -> Result<Box<dyn Transcriber>, AppError> {
            Ok(Box::new(TestTranscriber {
                responses: Arc::clone(&self.responses),
            }))
        }
    }

    struct TestTranscriber {
        responses: Arc<Mutex<VecDeque<Result<String, AppError>>>>,
    }

    impl Transcriber for TestTranscriber {
        fn transcribe(
            &self,
            _samples: &[f32],
            _language: Option<&str>,
        ) -> Result<String, AppError> {
            let next = self
                .responses
                .lock()
                .expect("transcriber responses lock")
                .pop_front();
            match next {
                Some(result) => result,
                None => Ok(String::new()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::test_support::{
        daemon_config, TestAudioBackend, TestOutput, TestTranscriberFactory,
    };

    #[test]
    fn daemon_loop_emits_transcript_to_output() -> Result<(), AppError> {
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
            let _ = control_sender.send(ControlEvent::StartRecording);
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(50));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 1: hello")));
        Ok(())
    }

    #[test]
    fn no_speech_timeout_cancels_silent_recording() -> Result<(), AppError> {
        let (sender, receiver) = mpsc::channel();
        let control_sender = sender.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut output = TestOutput::default();
        let deps = DaemonDeps {
            audio: Box::new(TestAudioBackend::new(
                vec!["Mic".to_string()],
                vec![vec![0.0; 100]],
            )),
            transcriber_factory: Box::new(TestTranscriberFactory::new(Vec::new())),
        };
        let config = DaemonConfig {
            sample_rate: 1_000,
            no_speech_timeout_ms: 30,
            ..daemon_config()
        };

        let shutdown_trigger = Arc::clone(&shutdown);
        let control_thread = thread::spawn(move || {
            let _ = control_sender.send(ControlEvent::StartRecording);
            thread::sleep(Duration::from_millis(80));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line == "No speech detected, cancelled."));
        Ok(())
    }

    #[test]
    fn sustained_speech_prevents_no_speech_cancellation() -> Result<(), AppError> {
        let (sender, receiver) = mpsc::channel();
        let control_sender = sender.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut output = TestOutput::default();
        let deps = DaemonDeps {
            audio: Box::new(TestAudioBackend::new(
                vec!["Mic".to_string()],
                vec![vec![0.2; 100]],
            )),
            transcriber_factory: Box::new(TestTranscriberFactory::new(vec!["speech".to_string()])),
        };
        let config = DaemonConfig {
            sample_rate: 1_000,
            no_speech_timeout_ms: 30,
            ..daemon_config()
        };

        let shutdown_trigger = Arc::clone(&shutdown);
        let control_thread = thread::spawn(move || {
            let _ = control_sender.send(ControlEvent::StartRecording);
            thread::sleep(Duration::from_millis(80));
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(20));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(!output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("No speech detected")));
        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 1: speech")));
        Ok(())
    }

    #[test]
    fn hold_recording_continuous_mode_transcribes_on_pause_before_release() -> Result<(), AppError>
    {
        let (sender, receiver) = mpsc::channel();
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
            vad: VadMode::Continuous,
            vad_silence_ms: 20,
            vad_chunk_ms: 20,
            ..daemon_config()
        };

        let shutdown_trigger = Arc::clone(&shutdown);
        let control_thread = thread::spawn(move || {
            let _ = control_sender.send(ControlEvent::StartRecording);
            thread::sleep(Duration::from_millis(120));
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(20));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 1: pause transcript")));
        Ok(())
    }

    #[test]
    fn continuous_mode_transcribes_long_speech_before_release() -> Result<(), AppError> {
        let (sender, receiver) = mpsc::channel();
        let control_sender = sender.clone();
        let (transcript_sender, transcript_receiver) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut output = SignalOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            transcript_sender,
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
            let _ = control_sender.send(ControlEvent::StartRecording);
            if transcript_receiver
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
            {
                transcript_before_release_trigger.store(true, Ordering::Relaxed);
            }
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(50));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stdout
            .iter()
            .any(|line| line.contains("Transcript 1: timed transcript")));
        assert!(
            transcript_before_release.load(Ordering::Relaxed),
            "expected timed continuous segmentation before key release"
        );
        Ok(())
    }

    #[test]
    fn carried_speech_overlap_is_deduped_on_following_silence_segment() -> Result<(), AppError> {
        let (sender, receiver) = mpsc::channel();
        let control_sender = sender.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut output = TestOutput::default();
        let deps = DaemonDeps {
            audio: Box::new(TestAudioBackend::new(
                vec!["Mic".to_string()],
                vec![vec![0.2; 20], vec![0.0; 5], vec![0.2; 20], vec![0.0; 20]],
            )),
            transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
                "hello world".to_string(),
                "world again".to_string(),
            ])),
        };
        let config = DaemonConfig {
            sample_rate: 1_000,
            vad: VadMode::Continuous,
            vad_silence_ms: 20,
            vad_chunk_ms: 20,
            segment_target_ms: 20,
            segment_grace_ms: 200,
            segment_overlap_ms: 10,
            segment_min_ms: 10,
            ..daemon_config()
        };

        let shutdown_trigger = Arc::clone(&shutdown);
        let control_thread = thread::spawn(move || {
            let _ = control_sender.send(ControlEvent::StartRecording);
            thread::sleep(Duration::from_millis(200));
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(50));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 1: hello world")));
        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 2: again")));
        assert!(!output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 2: world again")));
        Ok(())
    }

    #[test]
    fn carried_silence_overlap_does_not_dedupe_following_segment() -> Result<(), AppError> {
        let (sender, receiver) = mpsc::channel();
        let control_sender = sender.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut output = TestOutput::default();
        let deps = DaemonDeps {
            audio: Box::new(TestAudioBackend::new(
                vec!["Mic".to_string()],
                vec![vec![0.2; 20], vec![0.0; 5], vec![0.2; 20], vec![0.0; 20]],
            )),
            transcriber_factory: Box::new(TestTranscriberFactory::new(vec![
                "hello world".to_string(),
                "world again".to_string(),
            ])),
        };
        let config = DaemonConfig {
            sample_rate: 1_000,
            vad: VadMode::Continuous,
            vad_silence_ms: 20,
            vad_chunk_ms: 20,
            segment_target_ms: 20,
            segment_grace_ms: 200,
            segment_overlap_ms: 5,
            segment_min_ms: 10,
            ..daemon_config()
        };

        let shutdown_trigger = Arc::clone(&shutdown);
        let control_thread = thread::spawn(move || {
            let _ = control_sender.send(ControlEvent::StartRecording);
            thread::sleep(Duration::from_millis(200));
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(50));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 2: world again")));
        Ok(())
    }

    #[test]
    fn carried_overlap_speech_detection_uses_chunks() {
        let mut samples = vec![0.0; 50];
        samples.extend(vec![0.2; 10]);

        assert!(carried_overlap_contains_speech(&samples, 1_000, 0.15, 10));
        assert!(!carried_overlap_contains_speech(
            &vec![0.0; 60],
            1_000,
            0.15,
            10
        ));
    }

    #[test]
    fn failed_model_reload_keeps_existing_worker() -> Result<(), AppError> {
        let data_home = temp_data_home();
        let _data_guard = EnvGuard::set("XDG_DATA_HOME", &data_home);
        let model_path = data_home
            .join("soundvibes")
            .join("models")
            .join("ggml-small.en.bin");
        fs::create_dir_all(model_path.parent().expect("model parent")).expect("create model dir");
        fs::write(&model_path, b"test model").expect("write model file");

        let (sender, receiver) = mpsc::channel();
        let control_sender = sender.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut output = TestOutput::default();
        let deps = DaemonDeps {
            audio: Box::new(TestAudioBackend::new(
                vec!["Mic".to_string()],
                vec![vec![0.2; 160]],
            )),
            transcriber_factory: Box::new(ReloadFailFactory {
                responses: Arc::new(Mutex::new(vec![Ok("after failure".to_string())].into())),
                load_count: Arc::new(AtomicUsize::new(0)),
            }),
        };
        let config = daemon_config();

        let shutdown_trigger = Arc::clone(&shutdown);
        let control_thread = thread::spawn(move || {
            let _ = control_sender.send(ControlEvent::SetModel {
                size: ModelSize::Small,
                model_language: ModelLanguage::En,
            });
            thread::sleep(Duration::from_millis(50));
            let _ = control_sender.send(ControlEvent::StartRecording);
            let _ = control_sender.send(ControlEvent::StopRecording);
            thread::sleep(Duration::from_millis(50));
            shutdown_trigger.store(true, Ordering::Relaxed);
        });

        let result = run_daemon_loop(&config, &deps, &mut output, receiver, &shutdown);
        control_thread.join().expect("control thread failed");
        result?;

        assert!(output
            .stderr_lines()
            .iter()
            .any(|line| line.contains("Model reload failed")));
        assert!(output
            .stdout_lines()
            .iter()
            .any(|line| line.contains("Transcript 1: after failure")));
        Ok(())
    }

    struct SignalOutput {
        stdout: Vec<String>,
        stderr: Vec<String>,
        transcript_sender: mpsc::Sender<()>,
    }

    impl DaemonOutput for SignalOutput {
        fn stdout(&mut self, message: &str) {
            if message.contains("Transcript 1: timed transcript") {
                let _ = self.transcript_sender.send(());
            }
            self.stdout.push(message.to_string());
        }

        fn stderr(&mut self, message: &str) {
            self.stderr.push(message.to_string());
        }
    }

    struct ReloadFailFactory {
        responses: Arc<Mutex<VecDeque<Result<String, AppError>>>>,
        load_count: Arc<AtomicUsize>,
    }

    impl TranscriberFactory for ReloadFailFactory {
        fn load(&self, _model_path: Option<&Path>) -> Result<Box<dyn Transcriber>, AppError> {
            let load_number = self.load_count.fetch_add(1, AtomicOrdering::SeqCst);
            if load_number == 1 {
                return Err(AppError::runtime("planned reload failure"));
            }
            Ok(Box::new(ReloadFailTranscriber {
                responses: Arc::clone(&self.responses),
            }))
        }
    }

    struct ReloadFailTranscriber {
        responses: Arc<Mutex<VecDeque<Result<String, AppError>>>>,
    }

    impl Transcriber for ReloadFailTranscriber {
        fn transcribe(
            &self,
            _samples: &[f32],
            _language: Option<&str>,
        ) -> Result<String, AppError> {
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .unwrap_or_else(|| Ok(String::new()))
        }
    }

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

    fn temp_data_home() -> PathBuf {
        let mut dir = env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        dir.push(format!(
            "soundvibes-daemon-test-{}-{stamp}",
            std::process::id()
        ));
        dir
    }

    #[test]
    fn parses_set_model_command_tokens() {
        let (size, model_language) =
            parse_set_model_command("set-model size=small model-language=en")
                .expect("expected parse success");
        assert_eq!(size, ModelSize::Small);
        assert_eq!(model_language, ModelLanguage::En);
    }

    #[test]
    fn rejects_set_model_command_missing_language() {
        let err =
            parse_set_model_command("set-model size=small").expect_err("expected parse error");
        assert!(err.contains("model-language"));
    }
}
