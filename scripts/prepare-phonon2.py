#!/usr/bin/env python3
"""Fetch pinned model and native libraries. Only setup needs Python + zstandard."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import tarfile
import urllib.request
import zipfile

import zstandard

REVISION = "8ec0e26676d2d57149280e72dbeca922d482b61c"
MODEL_BASE = f"https://huggingface.co/FermionResearch/Phonon-2/resolve/{REVISION}"
ARCHIVE_HASH = "98125795b6dda72f5c6eee9ba33d19815df65dcb18b50a357bf9f73c9935309e"
CONTAINER_HASH = "4b6bfa3a12cc3c4e0a54f2ab3ec4ca7a842b09e5c7ecfc8e7ca0ac6cc8c11468"
NOTICE_HASH = "00624a5043e7ce74029317b024132ca5286116d6fbf3191d226374bc8273789f"
WHEEL_URL = "https://files.pythonhosted.org/packages/35/c3/90dc124286ce01004ddb30cd302381aad79fc281899d7dd2973ec85faef6/fermion_research-0.2.11-py3-none-any.whl"
WHEEL_HASH = "c4097574ef42e96caf31732097321da9e16c0b9108b5c616aa9d5d1f540319f8"


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def fetch(url, path, pin=None):
    if path.is_file() and pin and digest(path) == pin:
        return
    print(f"Downloading {path.name}", flush=True)
    part = path.with_suffix(path.suffix + ".part")
    try:
        with urllib.request.urlopen(url, timeout=120) as response, part.open("wb") as output:
            shutil.copyfileobj(response, output)
        if pin and digest(part) != pin:
            raise RuntimeError(f"Checksum mismatch: {path.name}")
        part.replace(path)
    finally:
        part.unlink(missing_ok=True)


def replace_bytes(path, data):
    # Replacing the inode preserves mappings held by running inference processes.
    part = path.with_name(path.name + ".part")
    try:
        part.write_bytes(data)
        part.replace(path)
    finally:
        part.unlink(missing_ok=True)


def main():
    data_home = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share")))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=data_home / "soundvibes/models/phonon2")
    parser.add_argument("--archive", type=Path, help="Reuse an already downloaded model archive")
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() not in {"x86_64", "aarch64"}:
        parser.error("Supported hosts: Linux x86_64 and aarch64")
    root = args.directory
    root.mkdir(parents=True, exist_ok=True)
    archive = args.archive or root / "phonon-2.bps.tar.zst"
    if args.archive:
        if digest(archive) != ARCHIVE_HASH:
            raise RuntimeError("Model archive checksum mismatch")
    else:
        fetch(f"{MODEL_BASE}/phonon-2.bps.tar.zst", archive, ARCHIVE_HASH)
    selected = {"config.json", "model.fermion", "bps_manifest.json", "packed_manifest.json"}
    with archive.open("rb") as source, zstandard.ZstdDecompressor().stream_reader(source) as stream, tarfile.open(fileobj=stream, mode="r|") as tar:
        for entry in tar:
            if entry.name in selected and entry.isfile():
                part = root / (entry.name + ".part")
                with tar.extractfile(entry) as source, part.open("wb") as output:
                    shutil.copyfileobj(source, output)
                if entry.name == "model.fermion" and digest(part) != CONTAINER_HASH:
                    part.unlink()
                    raise RuntimeError("Model container checksum mismatch")
                part.replace(root / entry.name)
                selected.remove(entry.name)
    if selected:
        raise RuntimeError(f"Missing archive entries: {selected}")
    config = json.loads((root / "config.json").read_text())
    if len(config["joint"]["vocabulary"]) != 8192:
        raise RuntimeError("Unexpected model vocabulary")
    wheel = root / "fermion_research-0.2.11-py3-none-any.whl"
    fetch(WHEEL_URL, wheel, WHEEL_HASH)
    libraries = root / "native"
    libraries.mkdir(exist_ok=True)
    arch = platform.machine()
    with zipfile.ZipFile(wheel) as package:
        for name in package.namelist():
            filename = Path(name).name
            if "_engine_phonon2_cpu/" in name and filename.startswith(("libphonon2_enc-", "libphonon2_tdt-")) and f"linux-{arch}" in filename and filename.endswith(".so"):
                replace_bytes(libraries / filename, package.read(name))
            if name.endswith("/LICENSE") or name.endswith("/NOTICE"):
                replace_bytes(root / ("fermion-" + filename), package.read(name))
    fetch(f"{MODEL_BASE}/NOTICE", root / "model-NOTICE", NOTICE_HASH)
    print(f"Prepared Phonon-2 at {root}\nNative libraries at {libraries}")


if __name__ == "__main__":
    main()
