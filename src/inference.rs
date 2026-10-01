use clap::ValueEnum;
use serde::Deserialize;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::daemon::TranscriberFactory;
use crate::error::AppError;
use crate::transcription_worker::Transcriber;
use crate::whisper::WhisperContext;

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InferenceEngine {
    #[default]
    Whisper,
    #[value(name = "phonon2")]
    Phonon2,
}

impl InferenceEngine {
    pub fn validate_input(self, sample_rate: u32, language: &str) -> Result<(), AppError> {
        if self == Self::Phonon2 {
            if sample_rate != 16_000 {
                return Err(AppError::config("Phonon-2 requires 16000 Hz mono audio"));
            }
            if !language.eq_ignore_ascii_case("en") && !language.eq_ignore_ascii_case("auto") {
                return Err(AppError::config(
                    "Phonon-2 supports English only; use language = \"en\" or \"auto\"",
                ));
            }
        }
        Ok(())
    }
}

pub struct InferenceFactory {
    engine: InferenceEngine,
    phonon2_library_dir: Option<PathBuf>,
}

impl InferenceFactory {
    pub fn new(engine: InferenceEngine, phonon2_library_dir: Option<PathBuf>) -> Self {
        Self {
            engine,
            phonon2_library_dir,
        }
    }
}

impl TranscriberFactory for InferenceFactory {
    fn load(&self, model_path: Option<&Path>) -> Result<Box<dyn Transcriber>, AppError> {
        let model_path = model_path.ok_or_else(|| AppError::config("model path is required"))?;
        match self.engine {
            InferenceEngine::Whisper => {
                let context = WhisperContext::from_file(model_path)
                    .map_err(|err| AppError::runtime(err.to_string()))?;
                Ok(Box::new(WhisperTranscriber { context }))
            }
            InferenceEngine::Phonon2 => {
                let library_dir = self
                    .phonon2_library_dir
                    .clone()
                    .unwrap_or_else(|| model_path.join("native"));
                let model = phonon2::Model::load(model_path, &library_dir)
                    .map_err(|err| AppError::runtime(format!("Phonon-2: {err}")))?;
                Ok(Box::new(Phonon2Transcriber {
                    model: Mutex::new(model),
                }))
            }
        }
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

struct Phonon2Transcriber {
    model: Mutex<phonon2::Model>,
}

impl Transcriber for Phonon2Transcriber {
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError> {
        InferenceEngine::Phonon2.validate_input(16_000, language.unwrap_or("auto"))?;
        self.model
            .lock()
            .map_err(|_| AppError::runtime("Phonon-2 model lock poisoned"))?
            .transcribe(samples)
            .map_err(|err| AppError::runtime(format!("Phonon-2: {err}")))
    }
}
