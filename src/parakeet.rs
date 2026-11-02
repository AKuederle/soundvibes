//! Persistent companion process isolates transcribe.cpp's ggml from Whisper's.
use clap::ValueEnum;
use serde::Deserialize;
use serde_json::{json, Value};

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use crate::error::AppError;
use crate::transcription_worker::Transcriber;

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParakeetBackend {
    #[default]
    Auto,
    Cpu,
    Vulkan,
}

struct Client {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<String>,
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Client {
    fn start(model: &Path, backend: ParakeetBackend) -> Result<Self, AppError> {
        let current = std::env::current_exe().map_err(failure)?;
        let directory = current
            .parent()
            .ok_or_else(|| failure("executable has no parent"))?;
        let directory = if directory.file_name().is_some_and(|name| name == "deps") {
            directory
                .parent()
                .ok_or_else(|| failure("test executable has no target directory"))?
        } else {
            directory
        };
        let executable = directory.join("sv-parakeet");
        let mut child = Command::new(executable)
            .arg(model)
            .arg(match backend {
                ParakeetBackend::Auto => "auto",
                ParakeetBackend::Cpu => "cpu",
                ParakeetBackend::Vulkan => "vulkan",
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                AppError::runtime(format!(
                    "start sv-parakeet companion: {e}; build/install both workspace binaries"
                ))
            })?;
        let input = child.stdin.take().ok_or_else(|| failure("missing stdin"))?;
        // A stalled native process must not block capture shutdown on a full pipe.
        // SAFETY: input owns this live file descriptor, and fcntl retains no pointers.
        let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure(std::io::Error::last_os_error()));
        }
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failure("missing stdout"))?;
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let client = Self {
            child,
            input,
            replies,
        };
        client.receive()?;
        Ok(client)
    }
    fn send(&mut self, request: Value) -> Result<(), AppError> {
        let mut bytes = serde_json::to_vec(&request).map_err(failure)?;
        bytes.push(b'\n');
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            match self.input.write(remaining) {
                Ok(0) => return Err(failure("companion pipe closed")),
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(failure("companion write timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(failure(error)),
            }
        }
        Ok(())
    }
    fn receive(&self) -> Result<String, AppError> {
        let line = self
            .replies
            .recv_timeout(Duration::from_secs(120))
            .map_err(failure)?;
        let reply: Value = serde_json::from_str(&line).map_err(failure)?;
        if let Some(error) = reply["error"].as_str() {
            return Err(failure(error));
        }
        reply["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| failure("invalid companion reply"))
    }
}
fn failure(error: impl std::fmt::Display) -> AppError {
    AppError::runtime(format!("Parakeet: {error}"))
}

pub struct ParakeetTranscriber {
    client: Mutex<Option<Client>>,
    model: PathBuf,
    backend: ParakeetBackend,
    words: Vec<String>,
    streaming: bool,
}
impl ParakeetTranscriber {
    pub fn load(model: &Path, backend: ParakeetBackend) -> Result<Self, AppError> {
        Ok(Self {
            client: Mutex::new(Some(Client::start(model, backend)?)),
            model: model.into(),
            backend,
            words: Vec::new(),
            streaming: false,
        })
    }
}
impl Transcriber for ParakeetTranscriber {
    fn set_vocabulary(&mut self, words: &[String]) -> Result<Option<String>, AppError> {
        self.words = words.to_vec();
        Ok(None)
    }
    fn begin_stream(&mut self, language: Option<&str>) -> Result<bool, AppError> {
        crate::inference::InferenceEngine::Parakeet
            .validate_input(16_000, language.unwrap_or("auto"))?;
        let mut guard = self.client.lock().map_err(failure)?;
        if guard.is_none() {
            *guard = Some(Client::start(&self.model, self.backend)?);
        }
        let result = match guard.as_mut() {
            Some(client) => client
                .send(json!({"op":"begin", "vocabulary":self.words}))
                .and_then(|_| client.receive())
                .map(|_| true),
            None => Err(failure("companion unavailable")),
        };
        if result.is_err() {
            *guard = None;
        }
        self.streaming = result.as_ref().is_ok_and(|active| *active);
        result
    }
    fn feed_stream(&mut self, samples: &[f32]) -> Result<(), AppError> {
        let mut guard = self.client.lock().map_err(failure)?;
        let result = guard
            .as_mut()
            .ok_or_else(|| failure("no active stream"))?
            .send(json!({"op":"feed", "samples":samples}));
        if result.is_err() {
            *guard = None;
        }
        result
    }
    fn finish_stream(&mut self) -> Result<String, AppError> {
        self.streaming = false;
        let mut guard = self.client.lock().map_err(failure)?;
        let result = match guard.as_mut() {
            Some(client) => client
                .send(json!({"op":"finish"}))
                .and_then(|_| client.receive()),
            None => Err(failure("no active stream")),
        };
        if result.is_err() {
            *guard = None;
        }
        result
    }
    fn cancel_stream(&mut self) {
        if !self.streaming {
            return;
        }
        self.streaming = false;
        // Cancel is consumed after queued feeds; no text is published.
        if let Ok(mut guard) = self.client.lock() {
            if let Some(client) = guard.as_mut() {
                if client.send(json!({"op":"cancel"})).is_err() {
                    *guard = None;
                }
            }
        }
    }
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError> {
        crate::inference::InferenceEngine::Parakeet
            .validate_input(16_000, language.unwrap_or("auto"))?;
        let mut guard = self.client.lock().map_err(failure)?;
        if guard.is_none() {
            *guard = Some(Client::start(&self.model, self.backend)?);
        }
        let result = match guard.as_mut() {
            Some(client) => client
                .send(json!({"op":"offline", "samples":samples, "vocabulary":self.words}))
                .and_then(|_| client.receive()),
            None => Err(failure("companion unavailable")),
        };
        if result.is_err() {
            *guard = None;
        }
        result
    }
}
