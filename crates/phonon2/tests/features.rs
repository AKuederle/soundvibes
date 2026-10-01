use phonon2::log_mel;

#[test]
fn matches_official_cpu_features_including_centered_trailing_frame() {
    let audio = std::io::Cursor::new(include_bytes!("fixtures/jfk-200ms.wav"));
    let mut reader = hound::WavReader::new(audio).expect("bundled audio");
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .take(3200)
        .map(|v| v.expect("sample") as f32 / 32768.0)
        .collect();
    // Independent oracle: fermion-research 0.2.6, symmetric Hann, BF16 disabled.
    let expected: Vec<f32> = include_bytes!("fixtures/jfk-200ms-log-mel.f32")
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect();
    let actual = log_mel(&samples).expect("extract features");
    assert_eq!(actual.frames, 21);
    assert_eq!(actual.values.len(), expected.len());
    assert!(
        actual.values.iter().all(|value| value.is_finite()),
        "non-finite feature output"
    );
    assert!(
        expected.iter().all(|value| value.is_finite()),
        "non-finite reference features"
    );
    let max_error = actual
        .values
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_error < 0.0002, "maximum feature error: {max_error}");
}

#[test]
fn silence_has_finite_features_and_invalid_audio_is_rejected() {
    let features = log_mel(&vec![0.0; 3200]).expect("silent audio");
    assert!(features
        .values
        .iter()
        .all(|v| v.is_finite() && v.abs() < 0.0001));
    assert!(log_mel(&[f32::NAN; 320]).is_err());
    assert!(log_mel(&[]).is_err());
}
