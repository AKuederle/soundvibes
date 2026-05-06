use clap::ValueEnum;
use serde::Deserialize;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::AppError;

const DEFAULT_MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

// VAD model constants
const VAD_MODEL_NAME: &str = "ggml-silero-v6.2.0.bin";
const VAD_MODEL_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin";
const VAD_MODEL_SIZE: u64 = 2_000_000; // ~2MB

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSize {
    Auto,
    Tiny,
    Base,
    Small,
    Medium,
    Large,
    #[serde(rename = "large-v3-turbo")]
    #[value(name = "large-v3-turbo")]
    LargeV3Turbo,
}

impl ModelSize {
    fn resolved(self) -> Self {
        match self {
            ModelSize::Auto => ModelSize::Small,
            other => other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ModelSize::Auto => "small",
            ModelSize::Tiny => "tiny",
            ModelSize::Base => "base",
            ModelSize::Small => "small",
            ModelSize::Medium => "medium",
            ModelSize::Large => "large",
            ModelSize::LargeV3Turbo => "large-v3-turbo",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelLanguage {
    Auto,
    En,
}

pub fn model_language_for_transcription(language: &str) -> ModelLanguage {
    if language.eq_ignore_ascii_case("auto") {
        ModelLanguage::Auto
    } else if language.eq_ignore_ascii_case("en") {
        ModelLanguage::En
    } else {
        ModelLanguage::Auto
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ModelSpec {
    pub size: ModelSize,
    pub language: ModelLanguage,
}

impl ModelSpec {
    pub fn new(size: ModelSize, language: ModelLanguage) -> Self {
        Self { size, language }
    }

    pub fn filename_result(&self) -> Result<String, AppError> {
        let size = self.size.resolved().as_str();
        match self.language {
            ModelLanguage::Auto => Ok(format!("ggml-{size}.bin")),
            ModelLanguage::En if self.size.resolved() == ModelSize::LargeV3Turbo => {
                Err(AppError::config(
                    "large-v3-turbo.en is not available; use model_language = \"auto\"",
                ))
            }
            ModelLanguage::En => Ok(format!("ggml-{size}.en.bin")),
        }
    }

    pub fn filename(&self) -> String {
        self.filename_result()
            .expect("invalid model spec for filename")
    }
}

#[derive(Debug)]
pub struct PreparedModel {
    pub path: PathBuf,
    pub downloaded: bool,
}

pub fn prepare_model(
    explicit_path: Option<&Path>,
    spec: &ModelSpec,
    allow_download: bool,
) -> Result<PreparedModel, AppError> {
    let path = resolve_model_path_result(explicit_path, spec)?;
    let downloaded = ensure_model_available(&path, spec, allow_download)?;
    Ok(PreparedModel { path, downloaded })
}

pub fn resolve_model_path_result(
    explicit_path: Option<&Path>,
    spec: &ModelSpec,
) -> Result<PathBuf, AppError> {
    match explicit_path {
        Some(path) => Ok(path.to_path_buf()),
        None => Ok(default_model_dir().join(spec.filename_result()?)),
    }
}

pub fn resolve_model_path(explicit_path: Option<&Path>, spec: &ModelSpec) -> PathBuf {
    explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_model_dir().join(spec.filename()))
}

pub fn default_model_dir() -> PathBuf {
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    data_home.join("soundvibes").join("models")
}

fn ensure_model_available(
    path: &Path,
    spec: &ModelSpec,
    allow_download: bool,
) -> Result<bool, AppError> {
    if path.exists() {
        validate_model_path(path)?;
        return Ok(false);
    }

    if !allow_download {
        return Err(AppError::config(format!(
            "model file not found at {} (set download_model = true to download)",
            path.display()
        )));
    }

    download_model(path, spec)?;
    validate_model_path(path)?;
    Ok(true)
}

fn validate_model_path(path: &Path) -> Result<(), AppError> {
    if !path.exists() {
        return Err(AppError::config(format!(
            "model file not found at {}",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(AppError::config(format!(
            "model path is not a file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn download_model(path: &Path, spec: &ModelSpec) -> Result<(), AppError> {
    let filename = spec.filename_result()?;
    let base = env::var("SV_MODEL_BASE_URL").unwrap_or_else(|_| DEFAULT_MODEL_BASE_URL.to_string());
    let url = format!("{}/{}", base.trim_end_matches('/'), filename);

    println!("Downloading model {filename} from {url}...");

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::config(format!(
                "failed to create model directory {}: {err}",
                parent.display()
            ))
        })?;
    }

    let response = ureq::get(&url)
        .call()
        .map_err(|err| AppError::config(format!("failed to download model from {url}: {err}")))?;
    if response.status() != 200 {
        return Err(AppError::config(format!(
            "model download failed with status {} from {url}",
            response.status()
        )));
    }

    let temp_path = path.with_extension("bin.part");
    let mut reader = response.into_reader();
    let mut file = fs::File::create(&temp_path).map_err(|err| {
        AppError::config(format!(
            "failed to create temporary model file {}: {err}",
            temp_path.display()
        ))
    })?;
    io::copy(&mut reader, &mut file)
        .map_err(|err| AppError::config(format!("failed to write model file: {err}")))?;
    file.flush()
        .map_err(|err| AppError::config(format!("failed to flush model file: {err}")))?;
    fs::rename(&temp_path, path).map_err(|err| {
        AppError::config(format!(
            "failed to move model file into place {}: {err}",
            path.display()
        ))
    })?;
    Ok(())
}

pub fn vad_model_path() -> PathBuf {
    default_model_dir().join(VAD_MODEL_NAME)
}

pub fn ensure_vad_model() -> Result<PathBuf, AppError> {
    let path = vad_model_path();

    if path.exists() {
        return Ok(path);
    }

    download_vad_model(&path)?;
    Ok(path)
}

fn download_vad_model(path: &Path) -> Result<(), AppError> {
    println!("Downloading VAD model from {VAD_MODEL_URL}...");

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::config(format!(
                "failed to create model directory {}: {err}",
                parent.display()
            ))
        })?;
    }

    let response = ureq::get(VAD_MODEL_URL)
        .call()
        .map_err(|err| AppError::config(format!("failed to download VAD model: {err}")))?;
    if response.status() != 200 {
        return Err(AppError::config(format!(
            "VAD model download failed with status {}",
            response.status()
        )));
    }

    let temp_path = path.with_extension("bin.part");
    let mut reader = response.into_reader();
    let mut file = fs::File::create(&temp_path).map_err(|err| {
        AppError::config(format!(
            "failed to create temporary VAD model file {}: {err}",
            temp_path.display()
        ))
    })?;
    io::copy(&mut reader, &mut file)
        .map_err(|err| AppError::config(format!("failed to write VAD model file: {err}")))?;
    file.flush()
        .map_err(|err| AppError::config(format!("failed to flush VAD model file: {err}")))?;
    fs::rename(&temp_path, path).map_err(|err| {
        AppError::config(format!(
            "failed to move VAD model file into place {}: {err}",
            path.display()
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_v3_turbo_uses_whisper_cpp_turbo_filename() {
        let spec = ModelSpec::new(ModelSize::LargeV3Turbo, ModelLanguage::Auto);

        assert_eq!(spec.filename(), "ggml-large-v3-turbo.bin");
    }

    #[test]
    fn large_v3_turbo_rejects_english_only_variant() {
        let spec = ModelSpec::new(ModelSize::LargeV3Turbo, ModelLanguage::En);

        let err = spec
            .filename_result()
            .expect_err("turbo has no English-only model");

        assert!(err.to_string().contains("large-v3-turbo.en"));
    }
}
