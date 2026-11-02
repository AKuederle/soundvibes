#!/usr/bin/env python3
"""Download or verify the pinned NVIDIA Parakeet Unified EN Q8_0 GGUF."""
import argparse
import hashlib
import os
from pathlib import Path
import tempfile
import urllib.request

REVISION = "7e948f21b7bdbac698d3318db9d350f1096f3b6c"
FILENAME = "parakeet-unified-en-0.6b-Q8_0.gguf"
SHA256 = "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795"
SIZE = 731357568


def verify(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    if path.stat().st_size != SIZE or digest.hexdigest() != SHA256:
        raise ValueError(f"{path}: size or SHA256 mismatch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--path", type=Path, default=Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "soundvibes/models" / FILENAME)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    if args.path.exists() or args.verify_only:
        verify(args.path)
    else:
        args.path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=args.path.parent, delete=False) as target:
            temporary = Path(target.name)
        try:
            urllib.request.urlretrieve(f"https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/{REVISION}/{FILENAME}", temporary)
            verify(temporary)
            temporary.replace(args.path)
        finally:
            temporary.unlink(missing_ok=True)
    print(args.path)


if __name__ == "__main__":
    main()
