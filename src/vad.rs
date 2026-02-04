//! Voice Activity Detection using whisper.cpp's Silero VAD model.

use std::ffi::CString;
use std::path::Path;
use std::ptr::NonNull;

use crate::whisper::bindings::*;

#[derive(Debug)]
pub enum VadError {
    InitFailed,
    InvalidPath,
}

impl std::fmt::Display for VadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VadError::InitFailed => write!(f, "failed to initialize VAD context"),
            VadError::InvalidPath => write!(f, "invalid VAD model path"),
        }
    }
}

impl std::error::Error for VadError {}

pub struct VadContext {
    ctx: NonNull<whisper_vad_context>,
}

impl VadContext {
    pub fn from_file(path: &Path) -> Result<Self, VadError> {
        let path_c = CString::new(path.to_str().ok_or(VadError::InvalidPath)?)
            .map_err(|_| VadError::InvalidPath)?;

        let params = unsafe { whisper_vad_default_context_params() };
        let ctx = unsafe { whisper_vad_init_from_file_with_params(path_c.as_ptr(), params) };
        let ctx = NonNull::new(ctx).ok_or(VadError::InitFailed)?;

        Ok(Self { ctx })
    }

    /// Detect speech segments in audio samples.
    /// Returns Vec of (start_sec, end_sec) tuples.
    pub fn detect_segments(
        &self,
        samples: &[f32],
        min_silence_ms: u32,
    ) -> Vec<(f32, f32)> {
        let mut params = unsafe { whisper_vad_default_params() };
        params.min_silence_duration_ms = min_silence_ms as i32;

        let segments = unsafe {
            whisper_vad_segments_from_samples(
                self.ctx.as_ptr(),
                params,
                samples.as_ptr(),
                samples.len() as i32,
            )
        };

        if segments.is_null() {
            return Vec::new();
        }

        let n = unsafe { whisper_vad_segments_n_segments(segments) };
        let mut result = Vec::with_capacity(n as usize);

        for i in 0..n {
            let t0 = unsafe { whisper_vad_segments_get_segment_t0(segments, i) };
            let t1 = unsafe { whisper_vad_segments_get_segment_t1(segments, i) };
            result.push((t0, t1));
        }

        unsafe { whisper_vad_free_segments(segments) };
        result
    }
}

impl Drop for VadContext {
    fn drop(&mut self) {
        unsafe { whisper_vad_free(self.ctx.as_ptr()) };
    }
}

// Safety: VadContext is thread-safe for read operations
unsafe impl Send for VadContext {}
unsafe impl Sync for VadContext {}
