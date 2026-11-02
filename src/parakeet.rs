//! Native Parakeet inference on the transcription worker, with recording-scoped streams.
use clap::ValueEnum;
use serde::Deserialize;
use transcribe_cpp::{
    Backend, Model, ModelOptions, RunOptions, Session, SessionOptions, Stream, StreamOptions,
};

use std::path::Path;
use std::sync::Mutex;

use crate::error::AppError;
use crate::inference::InferenceEngine;
use crate::transcription_worker::{StreamingTranscriber, Transcriber};

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParakeetBackend {
    #[default]
    Auto,
    Cpu,
    Vulkan,
}

fn failure(error: impl std::fmt::Display) -> AppError {
    AppError::runtime(format!("Parakeet: {error}"))
}

pub struct ParakeetTranscriber {
    session: Mutex<Session>,
    words: Vec<String>,
}
impl ParakeetTranscriber {
    pub fn load(path: &Path, backend: ParakeetBackend) -> Result<Self, AppError> {
        let model = Model::load_with(
            path,
            &ModelOptions {
                backend: match backend {
                    ParakeetBackend::Auto => Backend::Auto,
                    ParakeetBackend::Cpu => Backend::Cpu,
                    ParakeetBackend::Vulkan => Backend::Vulkan,
                },
                ..Default::default()
            },
        )
        .map_err(failure)?;
        if model.arch() != "parakeet" || model.variant() != "unified-en-0.6b" {
            return Err(failure(format!(
                "expected Parakeet Unified EN 0.6B, got {} / {}",
                model.arch(),
                model.variant()
            )));
        }
        eprintln!("Parakeet backend: {}", model.backend());
        let session = model
            .session_with(&SessionOptions {
                n_threads: if model.backend().eq_ignore_ascii_case("cpu") {
                    4
                } else {
                    1
                },
                ..Default::default()
            })
            .map_err(failure)?;
        Ok(Self {
            session: Mutex::new(session),
            words: Vec::new(),
        })
    }
    fn options(&self, language: Option<&str>) -> Result<RunOptions, AppError> {
        InferenceEngine::Parakeet.validate_input(16_000, language.unwrap_or("auto"))?;
        Ok(RunOptions {
            language: Some("en".into()),
            vocabulary: self.words.clone(),
            ..Default::default()
        })
    }
}
impl Transcriber for ParakeetTranscriber {
    fn set_vocabulary(&mut self, words: &[String]) -> Result<Option<String>, AppError> {
        self.words = words.to_vec();
        Ok(None)
    }
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError> {
        self.session
            .lock()
            .map_err(failure)?
            .run(samples, &self.options(language)?)
            .map(|t| t.text)
            .map_err(failure)
    }
    fn begin_stream(
        &mut self,
        language: Option<&str>,
    ) -> Result<Option<Box<dyn StreamingTranscriber + '_>>, AppError> {
        let options = self.options(language)?;
        let session = self.session.get_mut().map_err(failure)?;
        let stream = session
            .stream(&options, &StreamOptions::default())
            .map_err(failure)?;
        Ok(Some(Box::new(ParakeetStream(stream))))
    }
}
struct ParakeetStream<'a>(Stream<'a>);
impl StreamingTranscriber for ParakeetStream<'_> {
    fn feed(&mut self, samples: &[f32]) -> Result<(), AppError> {
        self.0.feed(samples).map(|_| ()).map_err(failure)
    }
    fn finish(&mut self) -> Result<String, AppError> {
        self.0.finalize().map_err(failure)?;
        Ok(self.0.text().full)
    }
}
