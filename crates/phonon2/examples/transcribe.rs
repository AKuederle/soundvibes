use std::path::PathBuf;
use std::time::Instant;

use phonon2::Model;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: transcribe MODEL_DIRECTORY LIBRARY_DIRECTORY AUDIO.wav".into());
    }
    let mut reader = hound::WavReader::open(PathBuf::from(&args[2]))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000
        || spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err("audio must be a 16-bit, 16 kHz mono WAV".into());
    }
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    let started = Instant::now();
    let mut model = Model::load(&PathBuf::from(&args[0]), &PathBuf::from(&args[1]))?;
    eprintln!("Loaded in {:.3}s", started.elapsed().as_secs_f64());
    let started = Instant::now();
    println!("{}", model.transcribe(&samples)?);
    eprintln!(
        "Transcribed {:.2}s of audio in {:.3}s",
        samples.len() as f64 / 16000.0,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
