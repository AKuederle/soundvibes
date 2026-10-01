#!/usr/bin/env python3
"""Record the official 0.2.6 CPU transcript and front end for comparison with Rust."""
import argparse
import json
import os
from pathlib import Path
import time

os.environ.setdefault("FERMION_CPU_THREADS", "4")
os.environ.setdefault("FERMION_CPU_BF16", "0")
os.environ.setdefault("FERMION_P2_PLANE_CACHE", "0")

import soundfile as sf
from fermion._speech.engine_phonon2_cpu import load


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("model", type=Path)
    parser.add_argument("audio", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    model = load(args.model, profile="phonon-2", backend="cpu")
    if model._cenc is None or model._ctdt is None:
        raise RuntimeError("Reference must use both native encoder and native TDT")
    samples, rate = sf.read(args.audio, dtype="float32")
    if rate != 16000 or samples.ndim != 1:
        raise ValueError("Provide 16 kHz mono audio")
    started = time.perf_counter()
    text = model._decode_single(samples, 1.0)[0]
    elapsed = time.perf_counter() - started
    features, _ = model._log_mel(samples)
    feature_path = args.output.with_suffix(".f32")
    features[0].numpy().astype("<f4").tofile(feature_path)
    args.output.write_text(json.dumps({"transcript": text, "seconds": elapsed, "frames": features.shape[1], "runtime": model.describe()}, indent=2))
    print(text)


if __name__ == "__main__":
    main()
