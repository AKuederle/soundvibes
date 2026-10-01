//! Phonon-2 inference with the original model and native CPU libraries.

mod container;
mod features;
mod native;

pub use features::{log_mel, Features};

use std::path::Path;
use std::sync::Mutex;

use container::Container;
use native::Native;

type Result<T> = std::result::Result<T, Error>;

static NATIVE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub(crate) fn path(path: &Path, error: impl std::fmt::Display) -> Self {
        Self(format!("{}: {error}", path.display()))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<libloading::Error> for Error {
    fn from(error: libloading::Error) -> Self {
        Self(error.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Original Phonon-2 weights running on the pinned native CPU engine.
///
/// Instances can move to a worker thread. Native calls across all instances are
/// serialized because upstream libraries share process-wide thread pools.
pub struct Model {
    native: Option<Native>,
    projector: Vec<f32>,
    projector_bias: Vec<f32>,
    vocabulary: Vec<String>,
}

impl Model {
    /// Load an unpacked model directory and libraries from fermion-research 0.2.6.
    /// No downloads occur here. See scripts/prepare-phonon2.py for asset setup.
    pub fn load(model_dir: &Path, library_dir: &Path) -> Result<Self> {
        let config = model_dir.join("config.json");
        let bytes = std::fs::read(&config).map_err(|e| Error::path(&config, e))?;
        let config: serde_json::Value = serde_json::from_slice(&bytes)?;
        let vocabulary: Vec<String> = serde_json::from_value(
            config
                .get("vocabulary")
                .or_else(|| {
                    config
                        .get("joint")
                        .and_then(|joint| joint.get("vocabulary"))
                })
                .ok_or_else(|| Error::new("config.json needs the archive's runtime vocabulary"))?
                .clone(),
        )?;
        if vocabulary.len() != 8192 {
            return Err(Error::new("Phonon-2 requires its 8192-entry vocabulary"));
        }
        let container = Container::read(&model_dir.join("model.fermion"))?;
        let projector = container.dense("encoder_projector.weight", &[640, 1024])?;
        let projector_bias = container.dense("encoder_projector.bias", &[640])?;
        let _lock = NATIVE_LOCK
            .lock()
            .map_err(|_| Error::new("native engine lock poisoned"))?;
        let native = Native::load(library_dir, &container)?;
        Ok(Self {
            native: Some(native),
            projector,
            projector_bias,
            vocabulary,
        })
    }

    /// Transcribe finite mono f32 PCM at 16 kHz. Phonon-2 supports English only.
    /// Empty audio or tails shorter than 10 ms return empty text. Longer recordings
    /// are decoded in consecutive windows of at most 35 seconds.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        if samples.iter().any(|v| !v.is_finite()) {
            return Err(Error::new("audio contains non-finite samples"));
        }
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| Error::new("native engine unavailable"))?;
        let _lock = NATIVE_LOCK
            .lock()
            .map_err(|_| Error::new("native engine lock poisoned"))?;
        let mut chunks = Vec::new();
        for samples in samples.chunks(35 * 16_000).filter(|s| s.len() >= 160) {
            let features = log_mel(samples)?;
            let encoder = native.encode(&features.values, features.frames)?;
            let frames = encoder.len() / 1024;
            let mut projected = vec![0.0; frames * 640];
            // SAFETY: A is frames*1024, B is the transpose of the stored 640*1024
            // projector, and C is frames*640. The allocations do not alias.
            unsafe {
                matrixmultiply::sgemm(
                    frames,
                    1024,
                    640,
                    1.0,
                    encoder.as_ptr(),
                    1024,
                    1,
                    self.projector.as_ptr(),
                    1,
                    1024,
                    0.0,
                    projected.as_mut_ptr(),
                    640,
                    1,
                );
            }
            for row in projected.as_chunks_mut::<640>().0 {
                for (value, bias) in row.iter_mut().zip(&self.projector_bias) {
                    *value += bias;
                }
            }
            let tokens = native.decode(&projected)?;
            let mut text = String::new();
            for token in tokens {
                if token == 8192 {
                    continue;
                }
                let piece = self
                    .vocabulary
                    .get(token as usize)
                    .ok_or_else(|| Error::new(format!("invalid decoded token: {token}")))?;
                if !(piece.starts_with("<|") && piece.ends_with("|>")
                    || piece == "<unk>"
                    || piece == "<pad>")
                {
                    text.push_str(piece);
                }
            }
            let text = text.replace('▁', " ").trim().to_string();
            if !text.is_empty() {
                chunks.push(text);
            }
        }
        Ok(chunks.join(" "))
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        let _lock = NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(self.native.take());
    }
}
