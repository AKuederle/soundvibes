//! Persistent companion process isolates transcribe.cpp's ggml from Whisper's.
use clap::ValueEnum;
use serde::Deserialize;
use serde_json::{json, Value};

use std::io::{BufRead, BufReader, Write};
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
        let executable = std::env::current_exe()
            .map_err(failure)?
            .with_file_name("sv-parakeet");
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
        writeln!(self.input, "{request}").map_err(failure)?;
        self.input.flush().map_err(failure)
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
}
impl ParakeetTranscriber {
    pub fn load(model: &Path, backend: ParakeetBackend) -> Result<Self, AppError> {
        Ok(Self {
            client: Mutex::new(Some(Client::start(model, backend)?)),
            model: model.into(),
            backend,
            words: Vec::new(),
        })
    }
}
impl Transcriber for ParakeetTranscriber {
    fn set_vocabulary(&mut self, words: &[String]) -> Result<Option<String>, AppError> {
        self.words = words.to_vec();
        Ok(None)
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
