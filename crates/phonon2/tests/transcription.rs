use std::env;
use std::path::Path;

use phonon2::{log_mel, Model};

fn words(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn missing_model_returns_a_path_error() {
    let error = Model::load(
        Path::new("/nonexistent/phonon2"),
        Path::new("/nonexistent/libs"),
    )
    .err()
    .expect("missing model must fail");
    assert!(error.to_string().contains("config.json"), "{error}");
}

#[test]
fn transcribes_real_audio_without_python() -> Result<(), Box<dyn std::error::Error>> {
    let Some(model_dir) = env::var_os("PHONON2_MODEL_DIR") else {
        eprintln!("Skipping real model test; set PHONON2_MODEL_DIR and PHONON2_LIBRARY_DIR.");
        return Ok(());
    };
    let library_dir =
        env::var_os("PHONON2_LIBRARY_DIR").ok_or("PHONON2_LIBRARY_DIR is required")?;
    let mut model = Model::load(Path::new(&model_dir), Path::new(&library_dir))?;
    let audio = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/jfk.wav");
    let mut reader = hound::WavReader::open(audio)?;
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.spec().channels, 1);
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    let text = model.transcribe(&samples)?;
    println!("Phonon-2: {text}");
    assert!(
        text.to_lowercase()
            .contains("ask not what your country can do for you"),
        "{text}"
    );
    assert!(
        text.to_lowercase()
            .contains("ask what you can do for your country"),
        "{text}"
    );
    if let Some(reference) = env::var_os("PHONON2_REFERENCE_JSON") {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(reference)?)?;
        let reference = value["transcript"]
            .as_str()
            .ok_or("reference transcript missing")?;
        assert_eq!(
            words(&text),
            words(reference),
            "word parity with official CPU engine"
        );
        if text != reference {
            println!("Reference punctuation differs: {reference}");
        }
    }
    if let Some(path) = env::var_os("PHONON2_REFERENCE_FEATURES") {
        let expected = std::fs::read(path)?;
        let features = log_mel(&samples)?;
        assert!(features.values.iter().all(|value| value.is_finite()));
        assert_eq!(features.values.len() * 4, expected.len());
        let max_error = features
            .values
            .iter()
            .zip(expected.as_chunks::<4>().0)
            .map(|(a, b)| {
                let reference = f32::from_le_bytes(*b);
                assert!(reference.is_finite(), "non-finite reference feature");
                (a - reference).abs()
            })
            .fold(0.0f32, f32::max);
        println!("Maximum full-recording feature error: {max_error}");
        assert!(
            max_error < 0.0003,
            "full-recording feature parity: {max_error}"
        );
    }
    assert_eq!(
        model.transcribe(&samples)?,
        text,
        "decoder state must reset between utterances"
    );
    model.set_hotwords(&["Quillon".to_string()], 20.0)?;
    let biased = model.transcribe(&samples)?;
    assert!(biased.to_lowercase().contains("quillon"), "{biased}");
    assert!(model
        .set_hotwords(&vec!["term".to_string(); 26], 2.0)
        .is_err());
    assert!(model.set_hotwords(&[], f32::NAN).is_err());
    model.set_hotwords(&[], 2.0)?;
    assert_eq!(
        model.transcribe(&samples)?,
        text,
        "clearing hotwords must reset bias"
    );
    model.set_hotwords(&["Quillon".to_string()], 0.0)?;
    assert_eq!(
        model.transcribe(&samples)?,
        text,
        "zero strength disables bias"
    );
    assert_eq!(model.transcribe(&[])?, "");
    assert!(model.transcribe(&[f32::NAN; 320]).is_err());
    drop(model);
    let mut reloaded = Model::load(Path::new(&model_dir), Path::new(&library_dir))?;
    let result = std::thread::spawn(move || reloaded.transcribe(&samples))
        .join()
        .map_err(|_| "transcription worker panicked")??;
    assert_eq!(
        result, text,
        "loading after use must preserve native pool safety"
    );
    Ok(())
}
