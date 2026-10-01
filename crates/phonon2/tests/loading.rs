use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use phonon2::Model;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn loading_error(header: serde_json::Value, payload: &[u8]) -> String {
    let directory = std::env::temp_dir().join(format!(
        "phonon2-load-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).expect("directory");
    std::fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&serde_json::json!({"vocabulary": vec!["piece"; 8192]}))
            .expect("config"),
    )
    .expect("write config");
    let header = serde_json::to_vec(&header).expect("header");
    let mut container = (header.len() as u64).to_le_bytes().to_vec();
    container.extend(header);
    container.extend(payload);
    std::fs::write(directory.join("model.fermion"), container).expect("container");
    let error = Model::load(&directory, Path::new("/nonexistent/native"))
        .err()
        .expect("invalid persisted input must fail")
        .to_string();
    std::fs::remove_dir_all(directory).expect("cleanup");
    error
}

#[test]
fn rejects_truncated_and_overflowing_weight_records_before_native_loading() {
    let error = loading_error(
        serde_json::json!({"format":"fermion-five-value-parakeet-v1","index":[{"n":"encoder_projector.weight","k":"int6","shape":[640,1024],"b":492800}]}),
        &[],
    );
    assert!(error.contains("invalid tensor size"), "{error}");
    let error = loading_error(
        serde_json::json!({"format":"fermion-five-value-parakeet-v1","index":[{"n":"invalid","k":"fp16","shape":[usize::MAX,2],"b":0}]}),
        &[],
    );
    assert!(error.contains("overflow"), "{error}");
}

#[test]
fn rejects_incompatible_projector_shape_and_duplicate_tensor_names() {
    let entry = serde_json::json!({"n":"encoder_projector.weight","k":"fp16","shape":[1],"b":2});
    let error = loading_error(
        serde_json::json!({"format":"fermion-five-value-parakeet-v1","index":[entry.clone()]}),
        &[0, 0],
    );
    assert!(error.contains("invalid shape"), "{error}");
    let error = loading_error(
        serde_json::json!({"format":"fermion-five-value-parakeet-v1","index":[entry.clone(),entry]}),
        &[0, 0, 0, 0],
    );
    assert!(error.contains("duplicate tensor"), "{error}");
}
