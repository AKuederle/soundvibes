use std::ffi::{c_void, CStr, CString, NulError};
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

use regex::Regex;

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals
)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/whisper_bindings.rs"));
}

use bindings::*;

unsafe extern "C" {
    fn sv_whisper_full_configured(
        ctx: *mut whisper_context,
        samples: *const f32,
        n_samples: c_int,
        language: *const c_char,
        detect_language: bool,
        n_threads: c_int,
    ) -> c_int;
}

#[derive(Debug)]
pub enum WhisperError {
    InvalidPath(NulError),
    InitFailed,
    TranscriptionFailed(i32),
}

struct LogCapture {
    gpu_backend: Mutex<Option<String>>,
    saw_no_gpu: AtomicBool,
}

impl LogCapture {
    fn new() -> Self {
        Self {
            gpu_backend: Mutex::new(None),
            saw_no_gpu: AtomicBool::new(false),
        }
    }

    fn reset(&self) {
        if let Ok(mut backend) = self.gpu_backend.lock() {
            *backend = None;
        }
        self.saw_no_gpu.store(false, Ordering::Relaxed);
    }

    fn capture_line(&self, message: &str) {
        if message.contains("whisper_backend_init_gpu: no GPU found") {
            self.saw_no_gpu.store(true, Ordering::Relaxed);
            return;
        }

        if let Some((_, rest)) = message.split_once("whisper_backend_init_gpu: using ") {
            let backend = rest.trim().trim_end_matches(" backend");
            if let Ok(mut stored) = self.gpu_backend.lock() {
                if stored.is_none() {
                    *stored = Some(backend.to_string());
                }
            }
        }
    }

    fn summary(&self) -> String {
        let backend = self.gpu_backend.lock().ok().and_then(|value| value.clone());
        if let Some(backend) = backend {
            return format!("whisper: GPU backend selected: {backend}");
        }
        if self.saw_no_gpu.load(Ordering::Relaxed) {
            return "whisper: no GPU backend detected; using CPU".to_string();
        }
        "whisper: GPU backend selection not reported; using CPU".to_string()
    }
}

static LOG_CAPTURE: OnceLock<LogCapture> = OnceLock::new();

fn log_capture() -> &'static LogCapture {
    LOG_CAPTURE.get_or_init(LogCapture::new)
}

unsafe extern "C" fn whisper_log_callback(
    _level: ggml_log_level,
    text: *const c_char,
    user_data: *mut c_void,
) {
    if text.is_null() {
        return;
    }
    if _level == ggml_log_level_GGML_LOG_LEVEL_DEBUG {
        return;
    }
    let message = CStr::from_ptr(text).to_string_lossy();
    if !user_data.is_null() {
        if let Some(state) = (user_data as *const LogCapture).as_ref() {
            state.capture_line(&message);
        }
    }
    eprint!("{message}");
}

impl std::fmt::Display for WhisperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WhisperError::InvalidPath(error) => write!(f, "invalid model path: {error}"),
            WhisperError::InitFailed => write!(f, "failed to initialize whisper context"),
            WhisperError::TranscriptionFailed(code) => {
                write!(f, "whisper transcription failed with code {code}")
            }
        }
    }
}

impl std::error::Error for WhisperError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WhisperError::InvalidPath(error) => Some(error),
            _ => None,
        }
    }
}

pub struct WhisperContext {
    ctx: NonNull<whisper_context>,
}

unsafe impl Send for WhisperContext {}

impl WhisperContext {
    pub fn from_file(path: &Path) -> Result<Self, WhisperError> {
        let path_c =
            CString::new(path.as_os_str().as_bytes()).map_err(WhisperError::InvalidPath)?;
        let log_capture = log_capture();
        log_capture.reset();
        unsafe {
            whisper_log_set(
                Some(whisper_log_callback),
                log_capture as *const _ as *mut c_void,
            );
        }
        let mut params = unsafe { whisper_context_default_params() };
        params.use_gpu = true;
        params.flash_attn = false;
        params.gpu_device = 0;

        let ctx = unsafe { whisper_init_from_file_with_params(path_c.as_ptr(), params) };
        let ctx = NonNull::new(ctx).ok_or(WhisperError::InitFailed)?;
        eprintln!("{}", log_capture.summary());
        Ok(Self { ctx })
    }

    pub fn transcribe(
        &self,
        samples: &[f32],
        language: Option<&str>,
    ) -> Result<String, WhisperError> {
        let available_threads = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        let n_threads = (available_threads / 2).max(1) as i32;

        let detect_language = language.is_none();
        let language_cstring;
        let language_ptr = if let Some(language) = language {
            language_cstring = CString::new(language).map_err(WhisperError::InvalidPath)?;
            language_cstring.as_ptr()
        } else {
            std::ptr::null()
        };

        let result = unsafe {
            sv_whisper_full_configured(
                self.ctx.as_ptr(),
                samples.as_ptr(),
                samples.len() as i32,
                language_ptr,
                detect_language,
                n_threads,
            )
        };
        if result != 0 {
            return Err(WhisperError::TranscriptionFailed(result));
        }

        let segments = unsafe { whisper_full_n_segments(self.ctx.as_ptr()) };
        let mut output = String::new();
        for i in 0..segments {
            let text_ptr = unsafe { whisper_full_get_segment_text(self.ctx.as_ptr(), i) };
            if !text_ptr.is_null() {
                let text = unsafe { CStr::from_ptr(text_ptr) }.to_string_lossy();
                output.push_str(text.trim());
                output.push(' ');
            }
        }
        Ok(filter_nonverbal(&output))
    }
}

/// Remove non-verbal annotations like [no sound], (metal clinking), *sighs*, etc.
fn filter_nonverbal(text: &str) -> String {
    let re = Regex::new(r"\s*[\[\(][^\]\)]*[\]\)]|\s*\*[^*]+\*").unwrap();
    let filtered = re.replace_all(text, "");
    // Clean up extra whitespace
    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Drop for WhisperContext {
    fn drop(&mut self) {
        unsafe { whisper_free(self.ctx.as_ptr()) };
    }
}
