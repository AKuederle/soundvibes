use std::env;
use std::path::{Path, PathBuf};

use crate::error::AppError;

#[derive(Debug)]
pub struct PreparedModel {
    pub path: PathBuf,
}

pub fn prepare_phonon2_model(explicit_path: Option<&Path>) -> Result<PreparedModel, AppError> {
    let path = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_model_dir().join("phonon2"));
    for filename in ["config.json", "model.fermion"] {
        if !path.join(filename).is_file() {
            return Err(AppError::config(format!(
                "Phonon-2 model not found at {}: missing {filename}; prepare it with scripts/prepare-phonon2.py --directory {}",
                path.display(), path.display()
            )));
        }
    }
    Ok(PreparedModel { path })
}

fn default_model_dir() -> PathBuf {
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    data_home.join("soundvibes").join("models")
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

/// Use the separately prepared, checksum-verified Parakeet asset.
pub fn prepare_parakeet_model(explicit_path: Option<&Path>) -> Result<PreparedModel, AppError> {
    let path = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_model_dir().join("parakeet-unified-en-0.6b-Q8_0.gguf"));
    validate_model_path(&path).map_err(|_| AppError::config(format!("Parakeet model missing at {}; run scripts/prepare-parakeet.py or set --model to an existing GGUF", path.display())))?;
    Ok(PreparedModel { path })
}
