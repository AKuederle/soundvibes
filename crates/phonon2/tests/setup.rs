use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn preparing_assets_preserves_already_mapped_native_libraries(
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(archive) = env::var_os("PHONON2_MODEL_ARCHIVE") else {
        eprintln!("Skipping setup regression; set PHONON2_MODEL_ARCHIVE and PHONON2_MODEL_DIR.");
        return Ok(());
    };
    let model_dir = env::var_os("PHONON2_MODEL_DIR").ok_or("PHONON2_MODEL_DIR required")?;
    let python = env::var_os("PHONON2_SETUP_PYTHON").unwrap_or_else(|| "python3".into());
    let directory = env::temp_dir().join(format!("phonon2-setup-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    for name in ["fermion_research-0.2.6-py3-none-any.whl", "model-NOTICE"] {
        std::fs::copy(Path::new(&model_dir).join(name), directory.join(name))?;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/prepare-phonon2.py");
    let output = Command::new(python)
        .arg("-c")
        .arg(
            r#"
import ctypes, pathlib, platform, subprocess, sys
script, directory, archive = sys.argv[1:]
command = [sys.executable, script, '--directory', directory, '--archive', archive]
subprocess.run(command, check=True)
path = pathlib.Path(directory) / 'native' / f'libphonon2_enc-linux-{platform.machine()}-base.so'
library = ctypes.CDLL(str(path))
assert library.phonon2_enc_abi_version() == 1
before = path.stat().st_ino
subprocess.run(command, check=True)
assert path.stat().st_ino != before, 'setup rewrote a mapped native library in place'
assert library.phonon2_enc_abi_version() == 1
"#,
        )
        .arg(script)
        .arg(&directory)
        .arg(archive)
        .output()?;
    std::fs::remove_dir_all(directory)?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
