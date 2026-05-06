use chrono::{Local, Utc};
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::flag;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
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
    pub mode: OutputMode,
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
    pub debug_vad: bool,
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

pub trait Transcriber: Send {
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError>;
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
        _ => {
            let host_id = match audio_host {
                AudioHost::Alsa => cpal::HostId::Alsa,
                AudioHost::Default => cpal::HostId::Alsa,
            };
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    StartRecording,
    StopRecording,
    Stop,
    SetModel {
        size: ModelSize,
        model_language: ModelLanguage,
    },
    Error(String),
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
        config.debug_vad,
    );

    let mut recording = false;
    let mut buffer = Vec::new();
    let mut next_job_index = 1u64;
    let mut next_emit_index = 1u64;
    let mut pending_jobs = 0usize;
    let mut pending_results = BTreeMap::new();
    let mut last_emitted_transcript = String::new();
    let mut capture: Option<Box<dyn CaptureSource>> = None;
    // For continuous mode: track silence at end of buffer
    let mut trailing_silence_samples: usize = 0;
    // For no-speech timeout: track when recording started and sustained speech
    let mut recording_started: Option<std::time::Instant> = None;
    let mut speech_detected = false;
    let mut speech_samples: usize = 0;
    // Require 100ms of speech to count as "speech detected"
    let speech_confirm_samples = (0.1 * config.sample_rate as f32) as usize;
    // Grace period after recording starts to ignore audio feedback pickup (500ms)
    let speech_detection_grace_ms: u64 = if config.audio_feedback { 500 } else { 0 };
    let segment_config = segment_config(config);

    loop {
        drain_worker_results(
            &worker,
            config,
            output,
            &mut pending_results,
            &mut next_emit_index,
            &mut pending_jobs,
            &mut last_emitted_transcript,
        );

        if shutdown.load(Ordering::Relaxed) {
            if recording {
                stop_recording(
                    &worker,
                    config,
                    &vad,
                    &mut capture,
                    &mut buffer,
                    &mut next_job_index,
                    &mut pending_jobs,
                    output,
                )?;
            }
            wait_for_pending_results(
                &worker,
                config,
                output,
                &mut pending_results,
                &mut next_emit_index,
                &mut pending_jobs,
                &mut last_emitted_transcript,
            );
            worker.shutdown()?;
            output.stdout("Daemon shutting down.");
            break;
        }
        match control_events.recv_timeout(Duration::from_millis(20)) {
            Ok(ControlEvent::StartRecording) => {
                if !recording {
                    start_active_recording(
                        deps,
                        &host,
                        config,
                        &mut recording,
                        &mut capture,
                        &mut buffer,
                        &mut trailing_silence_samples,
                        &mut recording_started,
                        &mut speech_detected,
                        &mut speech_samples,
                        output,
                    )?;
                }
            }
            Ok(ControlEvent::StopRecording) => {
                if recording {
                    stop_active_recording(
                        &worker,
                        config,
                        &vad,
                        &mut recording,
                        &mut capture,
                        &mut buffer,
                        &mut next_job_index,
                        &mut pending_jobs,
                        output,
                    )?;
                    wait_for_pending_results(
                        &worker,
                        config,
                        output,
                        &mut pending_results,
                        &mut next_emit_index,
                        &mut pending_jobs,
                        &mut last_emitted_transcript,
                    );
                    output.stdout("Ready for next utterance.");
                    if config.audio_feedback {
                        feedback::play_stop_sound();
                    }
                }
            }
            Ok(ControlEvent::Stop) => {
                shutdown.store(true, Ordering::Relaxed);
            }
            Ok(ControlEvent::SetModel {
                size,
                model_language,
            }) => {
                if recording {
                    recording = false;
                    buffer.clear();
                    capture = None;
                    output.stdout("Recording stopped for model reload.");
                }
                wait_for_pending_results(
                    &worker,
                    config,
                    output,
                    &mut pending_results,
                    &mut next_emit_index,
                    &mut pending_jobs,
                    &mut last_emitted_transcript,
                );
                worker.shutdown()?;
                let spec = ModelSpec::new(size, model_language);
                match model::prepare_model(None, &spec, config.download_model) {
                    Ok(prepared) => {
                        if prepared.downloaded {
                            output.stdout("Model download complete.");
                        }
                        let new_path = prepared.path.clone();
                        match deps.transcriber_factory.load(Some(&new_path)) {
                            Ok(new_transcriber) => {
                                worker = TranscriptionWorker::start(new_transcriber);
                                output.stdout(&format!(
                                    "Model reloaded: size={}, model-language={}",
                                    model_size_token(size),
                                    model_language_token(model_language)
                                ));
                            }
                            Err(err) => {
                                output.stderr(&format!("Model reload failed: {err}"));
                            }
                        }
                    }
                    Err(err) => {
                        output.stderr(&format!("Model reload failed: {err}"));
                    }
                }
            }
            Ok(ControlEvent::Error(message)) => return Err(AppError::runtime(message)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(AppError::runtime("socket listener disconnected"))
            }
        }

        if recording {
            if let Some(active) = capture.as_mut() {
                let prev_len = buffer.len();
                active.drain(&mut buffer);
                let new_samples = buffer.len() - prev_len;

                // Check for speech in new samples
                if new_samples > 0 {
                    let new_audio = &buffer[prev_len..];
                    let rms = audio::rms_energy(new_audio);

                    // Track sustained speech for no-speech timeout (skip grace period for audio feedback)
                    let in_grace_period = recording_started
                        .map(|t| (t.elapsed().as_millis() as u64) < speech_detection_grace_ms)
                        .unwrap_or(false);

                    if !in_grace_period {
                        if rms >= config.vad_threshold {
                            speech_samples += new_samples;
                            if speech_samples >= speech_confirm_samples {
                                speech_detected = true;
                            }
                        } else {
                            // Reset on silence (require continuous speech)
                            speech_samples = 0;
                        }
                    }

                    if config.vad == VadMode::Continuous {
                        if rms < config.vad_threshold {
                            trailing_silence_samples += new_samples;
                        } else {
                            trailing_silence_samples = 0;
                        }

                        if let SegmentDecision::Cut { speech_end, reason } =
                            segmentation::decide_segment(
                                &segment_config,
                                buffer.len(),
                                trailing_silence_samples,
                                rms,
                            )
                        {
                            submit_segment(
                                &worker,
                                config,
                                &buffer[..speech_end],
                                reason != CutReason::Silence,
                                &mut next_job_index,
                                &mut pending_jobs,
                                output,
                            )?;
                            buffer = segmentation::carry_after_cut(
                                &buffer,
                                speech_end,
                                &segment_config,
                                reason,
                            );
                            trailing_silence_samples = 0;
                            speech_detected = false;
                            speech_samples = 0;
                            recording_started = Some(std::time::Instant::now());
                        }
                    }
                }

                // Check for no-speech timeout
                if config.no_speech_timeout_ms > 0
                    && !speech_detected
                    && recording_started
                        .map(|t| (t.elapsed().as_millis() as u64) >= config.no_speech_timeout_ms)
                        .unwrap_or(false)
                {
                    recording = false;
                    buffer.clear();
                    capture = None;
                    recording_started = None;
                    output.stdout("No speech detected, cancelled.");
                    if config.audio_feedback {
                        feedback::play_stop_sound();
                    }
                }
            }
        }
    }
    Ok(())
}

fn stop_recording(
    worker: &TranscriptionWorker,
    config: &DaemonConfig,
    vad: &audio::VadConfig,
    capture: &mut Option<Box<dyn CaptureSource>>,
    buffer: &mut Vec<f32>,
    next_job_index: &mut u64,
    pending_jobs: &mut usize,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    let mut active = capture
        .take()
        .ok_or_else(|| AppError::runtime("capture stream missing"))?;
    active.drain(buffer);
    submit_final_recording(
        worker,
        config,
        vad,
        buffer,
        next_job_index,
        pending_jobs,
        output,
    )?;
    Ok(())
}

fn start_active_recording(
    deps: &DaemonDeps,
    host: &cpal::Host,
    config: &DaemonConfig,
    recording: &mut bool,
    capture: &mut Option<Box<dyn CaptureSource>>,
    buffer: &mut Vec<f32>,
    trailing_silence_samples: &mut usize,
    recording_started: &mut Option<std::time::Instant>,
    speech_detected: &mut bool,
    speech_samples: &mut usize,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    let new_capture = deps
        .audio
        .start_capture(host, config.device.as_deref(), config.sample_rate)
        .map_err(|err| match err.kind {
            audio::AudioErrorKind::DeviceNotFound if config.device.is_some() => {
                AppError::audio(err.message)
            }
            _ => AppError::audio(err.message),
        })?;
    *recording = true;
    buffer.clear();
    *trailing_silence_samples = 0;
    *recording_started = Some(std::time::Instant::now());
    *speech_detected = false;
    *speech_samples = 0;
    *capture = Some(new_capture);
    output.stdout("Recording started.");
    if config.audio_feedback {
        feedback::play_start_sound();
    }
    Ok(())
}

fn stop_active_recording(
    worker: &TranscriptionWorker,
    config: &DaemonConfig,
    vad: &audio::VadConfig,
    recording: &mut bool,
    capture: &mut Option<Box<dyn CaptureSource>>,
    buffer: &mut Vec<f32>,
    next_job_index: &mut u64,
    pending_jobs: &mut usize,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    *recording = false;
    stop_recording(
        worker,
        config,
        vad,
        capture,
        buffer,
        next_job_index,
        pending_jobs,
        output,
    )
}

fn submit_final_recording(
    worker: &TranscriptionWorker,
    config: &DaemonConfig,
    vad: &audio::VadConfig,
    buffer: &[f32],
    next_job_index: &mut u64,
    pending_jobs: &mut usize,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    let trimmed = audio::trim_trailing_silence(buffer, config.sample_rate, vad);
    if trimmed.is_empty() {
        return Ok(());
    }
    submit_segment(
        worker,
        config,
        &trimmed,
        false,
        next_job_index,
        pending_jobs,
        output,
    )
}

fn submit_segment(
    worker: &TranscriptionWorker,
    config: &DaemonConfig,
    samples: &[f32],
    had_overlap: bool,
    next_job_index: &mut u64,
    pending_jobs: &mut usize,
    output: &mut dyn DaemonOutput,
) -> Result<(), AppError> {
    if samples.is_empty() {
        return Ok(());
    }

    if config.dump_audio {
        dump_audio_samples(samples, config.sample_rate, output)?;
    }
    let index = *next_job_index;
    *next_job_index += 1;
    *pending_jobs += 1;
    worker.submit(TranscriptionJob {
        index,
        samples: samples.to_vec(),
        duration_ms: audio::samples_to_ms(samples.len(), config.sample_rate),
        language: Some(config.language.clone()),
        had_overlap,
    })
}

fn drain_worker_results(
    worker: &TranscriptionWorker,
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    pending_results: &mut BTreeMap<u64, TranscriptionResult>,
    next_emit_index: &mut u64,
    pending_jobs: &mut usize,
    last_emitted_transcript: &mut String,
) {
    while let Some(result) = worker.try_recv() {
        pending_results.insert(result.index, result);
    }
    emit_ready_results(
        config,
        output,
        pending_results,
        next_emit_index,
        pending_jobs,
        last_emitted_transcript,
    );
}

fn wait_for_pending_results(
    worker: &TranscriptionWorker,
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    pending_results: &mut BTreeMap<u64, TranscriptionResult>,
    next_emit_index: &mut u64,
    pending_jobs: &mut usize,
    last_emitted_transcript: &mut String,
) {
    while *pending_jobs > 0 {
        drain_worker_results(
            worker,
            config,
            output,
            pending_results,
            next_emit_index,
            pending_jobs,
            last_emitted_transcript,
        );
        if *pending_jobs == 0 {
            break;
        }
        match worker.recv() {
            Some(result) => {
                pending_results.insert(result.index, result);
                emit_ready_results(
                    config,
                    output,
                    pending_results,
                    next_emit_index,
                    pending_jobs,
                    last_emitted_transcript,
                );
            }
            None => break,
        }
    }
}

fn emit_ready_results(
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    pending_results: &mut BTreeMap<u64, TranscriptionResult>,
    next_emit_index: &mut u64,
    pending_jobs: &mut usize,
    last_emitted_transcript: &mut String,
) {
    while let Some(result) = pending_results.remove(next_emit_index) {
        *next_emit_index += 1;
        *pending_jobs = pending_jobs.saturating_sub(1);
        match result.transcript {
            Ok(transcript) => {
                let text = if result.had_overlap && !last_emitted_transcript.trim().is_empty() {
                    segmentation::dedupe_boundary(last_emitted_transcript, &transcript)
                } else {
                    transcript
                };
                if !text.trim().is_empty() {
                    if let Err(err) = emit_transcript(
                        config,
                        output,
                        &text,
                        audio::SegmentInfo {
                            index: result.index,
                            duration_ms: result.duration_ms,
                        },
                    ) {
                        output.stderr(&format!("Failed to emit transcript: {err}"));
                    } else {
                        *last_emitted_transcript = text;
                    }
                }
            }
            Err(err) => output.stderr(&format!("Transcription error: {err}")),
        }
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

fn emit_transcript(
    config: &DaemonConfig,
    output: &mut dyn DaemonOutput,
    text: &str,
    info: audio::SegmentInfo,
) -> Result<(), String> {
    match config.mode {
        OutputMode::Stdout => emit_stdout(config.format, output, text, info),
        mode => {
            if let Err(err) = output::output_text(text, mode, &config.output) {
                output.stderr(&format!("warn: {err}; falling back to stdout"));
                emit_stdout(config.format, output, text, info)
            } else {
                Ok(())
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
) -> Result<(), String> {
    match format {
        OutputFormat::Plain => {
            output.stdout(&format!("Transcript {}: {}", info.index, text));
        }
        OutputFormat::Jsonl => {
            let escaped = json_escape(text);
            let timestamp = Utc::now().to_rfc3339();
            output.stdout(&format!(
                "{{\"type\":\"final\",\"utterance\":{},\"duration_ms\":{},\"timestamp\":\"{}\",\"text\":\"{}\"}}",
                info.index, info.duration_ms, timestamp, escaped
            ));
        }
    }
    Ok(())
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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
                        eprintln!("socket read error: {err}");
                        continue;
                    }
                    let command = buffer.trim();
                    if command == "record-start" {
                        let _ = socket_sender.send(ControlEvent::StartRecording);
                    } else if command == "record-stop" {
                        let _ = socket_sender.send(ControlEvent::StopRecording);
                    } else if command == "stop" {
                        let _ = socket_sender.send(ControlEvent::Stop);
                    } else if command.starts_with("set-model") {
                        match parse_set_model_command(command) {
                            Ok((size, model_language)) => {
                                let _ = socket_sender.send(ControlEvent::SetModel {
                                    size,
                                    model_language,
                                });
                            }
                            Err(message) => {
                                eprintln!("invalid set-model command: {message}");
                            }
                        }
                    } else {
                        eprintln!("unsupported daemon command: {command}");
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

fn parse_set_model_command(command: &str) -> Result<(ModelSize, ModelLanguage), String> {
    let mut size = None;
    let mut model_language = None;
    for token in command.split_whitespace().skip(1) {
        if let Some(value) = token.strip_prefix("size=") {
            size = Some(parse_model_size(value).ok_or_else(|| {
                format!("invalid model size '{value}' (expected auto|tiny|base|small|medium|large|large-v3-turbo)")
            })?);
        } else if let Some(value) = token.strip_prefix("model-language=") {
            model_language =
                Some(parse_model_language(value).ok_or_else(|| {
                    format!("invalid model language '{value}' (expected auto|en)")
                })?);
        }
    }

    let size = size.ok_or_else(|| "missing size=<SIZE>".to_string())?;
    let model_language =
        model_language.ok_or_else(|| "missing model-language=<LANG>".to_string())?;
    Ok((size, model_language))
}

fn parse_model_size(value: &str) -> Option<ModelSize> {
    match value {
        "auto" => Some(ModelSize::Auto),
        "tiny" => Some(ModelSize::Tiny),
        "base" => Some(ModelSize::Base),
        "small" => Some(ModelSize::Small),
        "medium" => Some(ModelSize::Medium),
        "large" => Some(ModelSize::Large),
        "large-v3-turbo" => Some(ModelSize::LargeV3Turbo),
        _ => None,
    }
}

fn parse_model_language(value: &str) -> Option<ModelLanguage> {
    match value {
        "auto" => Some(ModelLanguage::Auto),
        "en" => Some(ModelLanguage::En),
        _ => None,
    }
}

pub fn send_record_start_command() -> Result<(), AppError> {
    send_daemon_command("record-start")
}

pub fn send_record_stop_command() -> Result<(), AppError> {
    send_daemon_command("record-stop")
}

pub fn send_stop_command() -> Result<(), AppError> {
    send_daemon_command("stop")
}

pub fn send_set_model_command(
    size: ModelSize,
    model_language: ModelLanguage,
) -> Result<(), AppError> {
    let command = format!(
        "set-model size={} model-language={}",
        model_size_token(size),
        model_language_token(model_language)
    );
    send_daemon_command(&command)
}

fn send_daemon_command(command: &str) -> Result<(), AppError> {
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
    Ok(())
}

fn model_size_token(size: ModelSize) -> &'static str {
    match size {
        ModelSize::Auto => "auto",
        ModelSize::Tiny => "tiny",
        ModelSize::Base => "base",
        ModelSize::Small => "small",
        ModelSize::Medium => "medium",
        ModelSize::Large => "large",
        ModelSize::LargeV3Turbo => "large-v3-turbo",
    }
}

fn model_language_token(model_language: ModelLanguage) -> &'static str {
    match model_language {
        ModelLanguage::Auto => "auto",
        ModelLanguage::En => "en",
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
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    use super::{
        AudioBackend, CaptureSource, ControlEvent, DaemonOutput, Transcriber, TranscriberFactory,
    };
    use crate::audio::{AudioError, AudioErrorKind};
    use crate::error::AppError;

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

    pub fn control_channel() -> (mpsc::Sender<ControlEvent>, mpsc::Receiver<ControlEvent>) {
        mpsc::channel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use super::test_support::{
        control_channel, TestAudioBackend, TestOutput, TestTranscriberFactory,
    };
    use crate::segmentation::{
        DEFAULT_SEGMENT_GRACE_MS, DEFAULT_SEGMENT_MIN_MS, DEFAULT_SEGMENT_OVERLAP_MS,
        DEFAULT_SEGMENT_TARGET_MS,
    };

    #[test]
    fn daemon_loop_emits_transcript_to_output() -> Result<(), AppError> {
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
            segment_target_ms: DEFAULT_SEGMENT_TARGET_MS,
            segment_grace_ms: DEFAULT_SEGMENT_GRACE_MS,
            segment_overlap_ms: DEFAULT_SEGMENT_OVERLAP_MS,
            segment_min_ms: DEFAULT_SEGMENT_MIN_MS,
            debug_audio: false,
            debug_vad: false,
            dump_audio: false,
            audio_feedback: false,
            no_speech_timeout_ms: 0,
            hotkey: HotkeyConfig::default(),
        };

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
    fn hold_recording_continuous_mode_transcribes_on_pause_before_release() -> Result<(), AppError>
    {
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
            vad_chunk_ms: 20,
            segment_target_ms: DEFAULT_SEGMENT_TARGET_MS,
            segment_grace_ms: DEFAULT_SEGMENT_GRACE_MS,
            segment_overlap_ms: DEFAULT_SEGMENT_OVERLAP_MS,
            segment_min_ms: DEFAULT_SEGMENT_MIN_MS,
            debug_audio: false,
            debug_vad: false,
            dump_audio: false,
            audio_feedback: false,
            no_speech_timeout_ms: 0,
            hotkey: HotkeyConfig::default(),
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
        let (sender, receiver) = control_channel();
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
            model_path: None,
            download_model: false,
            language: "en".to_string(),
            device: None,
            audio_host: AudioHost::Default,
            sample_rate: 1_000,
            format: OutputFormat::Plain,
            mode: OutputMode::Stdout,
            output: OutputConfig::default(),
            vad: VadMode::Continuous,
            vad_silence_ms: 100,
            vad_threshold: 0.015,
            vad_chunk_ms: 20,
            segment_target_ms: 20,
            segment_grace_ms: 20,
            segment_overlap_ms: 5,
            segment_min_ms: 10,
            debug_audio: false,
            debug_vad: false,
            dump_audio: false,
            audio_feedback: false,
            no_speech_timeout_ms: 0,
            hotkey: HotkeyConfig::default(),
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
