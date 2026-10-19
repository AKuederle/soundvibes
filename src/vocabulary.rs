use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One snapshot per transcription. Failed reads retain the last complete list.
pub(crate) struct Vocabulary {
    directory: Option<PathBuf>,
    words: Vec<String>,
}

impl Vocabulary {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            words: Vec::new(),
        }
    }

    pub fn refresh(&mut self) -> (&[String], Option<String>) {
        let warning =
            self.directory
                .as_deref()
                .and_then(|directory| match read_directory(directory) {
                    Ok(words) => {
                        self.words = words;
                        None
                    }
                    Err(error) => Some(format!(
                        "vocabulary: {error}; keeping the previous vocabulary"
                    )),
                });
        (&self.words, warning)
    }
}

fn read_directory(directory: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(context(directory, error)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| context(directory, e))?;
        let path = entry.path();
        if path.extension().is_some_and(|s| s == "txt")
            && !entry.file_name().as_encoded_bytes().starts_with(b".")
            && entry.file_type().map_err(|e| context(&path, e))?.is_file()
        {
            files.push(path);
        }
    }
    files.sort();
    let mut seen = HashSet::new();
    let mut words = Vec::new();
    for path in files {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            // A different app may remove its file between listing and reading.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(context(&path, error)),
        };
        for (line_number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.contains('\0') {
                return Err(context(
                    &path,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("line {} contains a NUL byte", line_number + 1),
                    ),
                ));
            }
            let word = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if seen.insert(word.to_lowercase()) {
                words.push(word);
            }
        }
    }
    Ok(words)
}

fn context(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
