//! Persistent local Parakeet inference process.
use std::io::{self, BufRead, Write};
use transcribe_cpp::{Backend, Model, ModelOptions, RunOptions, SessionOptions, StreamOptions};

fn reply(result: Result<String, String>) {
    let value = match result {
        Ok(text) => serde_json::json!({"text": text}),
        Err(error) => serde_json::json!({"error": error}),
    };
    println!("{value}");
    let _ = io::stdout().flush();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let backend = match args.get(2).map(String::as_str) {
        Some("cpu") => Backend::Cpu,
        Some("vulkan") => Backend::Vulkan,
        _ => Backend::Auto,
    };
    let model = Model::load_with(
        args.get(1).ok_or("missing model path")?,
        &ModelOptions {
            backend,
            ..Default::default()
        },
    )?;
    if model.arch() != "parakeet" || model.variant() != "unified-en-0.6b" {
        return Err(format!(
            "expected Parakeet Unified EN 0.6B, got {} / {}",
            model.arch(),
            model.variant()
        )
        .into());
    }
    eprintln!("Parakeet backend: {}", model.backend());
    let mut session = model.session_with(&SessionOptions {
        n_threads: if model.backend().eq_ignore_ascii_case("cpu") {
            4
        } else {
            1
        },
        ..Default::default()
    })?;
    reply(Ok(String::new()));
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(line) = lines.next() {
        let request: serde_json::Value = serde_json::from_str(&line?)?;
        let options = RunOptions {
            language: Some("en".into()),
            vocabulary: serde_json::from_value(request["vocabulary"].clone()).unwrap_or_default(),
            ..Default::default()
        };
        if request["op"] == "offline" {
            let samples: Vec<f32> = serde_json::from_value(request["samples"].clone())?;
            reply(
                session
                    .run(&samples, &options)
                    .map(|t| t.text)
                    .map_err(|e| e.to_string()),
            );
        } else if request["op"] == "begin" {
            let mut stream = match session.stream(&options, &StreamOptions::default()) {
                Ok(stream) => {
                    reply(Ok(String::new()));
                    stream
                }
                Err(error) => {
                    reply(Err(error.to_string()));
                    continue;
                }
            };
            let mut failure = None;
            for line in lines.by_ref() {
                let request: serde_json::Value = serde_json::from_str(&line?)?;
                match request["op"].as_str() {
                    Some("feed") => {
                        let samples: Vec<f32> = serde_json::from_value(request["samples"].clone())?;
                        if failure.is_none() {
                            if let Err(error) = stream.feed(&samples) {
                                failure = Some(error.to_string());
                            }
                        }
                    }
                    Some("finish") => {
                        reply(match failure {
                            Some(error) => Err(error),
                            None => stream
                                .finalize()
                                .map(|_| stream.text().full)
                                .map_err(|e| e.to_string()),
                        });
                        break;
                    }
                    Some("cancel") => break,
                    _ => return Err("unexpected stream command".into()),
                }
            }
        } else {
            return Err("unexpected command".into());
        }
    }
    Ok(())
}
