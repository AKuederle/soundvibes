//! ABI 1 from fermion-research 0.2.6's ctypes bindings. See README for provenance.
use libloading::Library;
use sha2::{Digest, Sha256};

use std::ffi::{c_int, c_void};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Once, OnceLock};

use crate::container::{Container, Planes, Quantized};
use crate::{Error, Result};

type Ptr = *const c_void;
type Handle = *mut c_void;
type CreateMatrix = unsafe extern "C" fn(c_int, c_int, Ptr, Ptr, Ptr, Ptr, c_int) -> Handle;
type Destroy = unsafe extern "C" fn(Handle);
type CreateEncoder =
    unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int) -> Handle;
type SetSubsampling = unsafe extern "C" fn(
    Handle,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
) -> c_int;
type SetLayer = unsafe extern "C" fn(Handle, c_int, Ptr, Ptr) -> c_int;
type Forward = unsafe extern "C" fn(Handle, Ptr, c_int, *mut c_void) -> c_int;
type CreateDecoder = unsafe extern "C" fn(
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    Ptr,
    c_int,
    c_int,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
    Ptr,
) -> Handle;
type Decode = unsafe extern "C" fn(Handle, Ptr, c_int, *mut c_void, c_int) -> c_int;

// Upstream pools keep worker threads running after handles are destroyed. Never
// unload their executable code, or reset a pool that has already processed work.
static ENCODER_LIBRARY: OnceLock<Library> = OnceLock::new();
static DECODER_LIBRARY: OnceLock<Library> = OnceLock::new();
static THREAD_POOLS: Once = Once::new();

pub(crate) struct Native {
    encoder: Option<NonNull<c_void>>,
    decoder: Option<NonNull<c_void>>,
    matrices: Vec<NonNull<c_void>>,
    // Keep these allocations stable until the corresponding native handles are destroyed.
    planes: Vec<Planes>,
    quantized: Vec<Quantized>,
    half_vectors: Vec<Vec<u16>>,
    vectors: Vec<Vec<f32>>,
    durations: Box<[c_int; 5]>,
    destroy_encoder: Destroy,
    destroy_decoder: Destroy,
    destroy_matrix: Destroy,
    forward: Forward,
    out_len: unsafe extern "C" fn(Handle, c_int) -> c_int,
    decode: Decode,
    set_encoder_threads: unsafe extern "C" fn(c_int),
    set_decoder_threads: unsafe extern "C" fn(c_int),
}

// Handles may move between threads. All native operations, including destruction,
// are serialized by crate::NATIVE_LOCK because the libraries have global thread pools.
unsafe impl Send for Native {}

macro_rules! symbol {
    ($lib:expr, $name:literal, $ty:ty) => {{
        // SAFETY: these signatures are copied from the pinned ABI 1 ctypes bindings.
        unsafe { *$lib.get::<$ty>(concat!($name, "\0").as_bytes())? }
    }};
}

impl Native {
    pub fn load(directory: &Path, container: &Container) -> Result<Self> {
        let (enc_name, enc_pin, tdt_name, tdt_pin) = library_names()?;
        let encoder_library = verified_library(directory, enc_name, enc_pin, &ENCODER_LIBRARY)?;
        let decoder_library = verified_library(directory, tdt_name, tdt_pin, &DECODER_LIBRARY)?;
        for (lib, name) in [
            (&encoder_library, "phonon2_cpu_abi_version"),
            (&encoder_library, "phonon2_enc_abi_version"),
            (&decoder_library, "phonon2_tdt_abi_version"),
        ] {
            let function = unsafe { *lib.get::<unsafe extern "C" fn() -> c_int>(name.as_bytes())? };
            if unsafe { function() } != 1 {
                return Err(Error::new(format!("unsupported native ABI: {name}")));
            }
        }
        let create_matrix = symbol!(encoder_library, "phonon2_cpu_create", CreateMatrix);
        let enable_onedot = symbol!(
            encoder_library,
            "phonon2_cpu_enable_onedot",
            unsafe extern "C" fn(Handle) -> f32
        );
        let create_encoder = symbol!(encoder_library, "phonon2_enc_create", CreateEncoder);
        let set_subsampling = symbol!(
            encoder_library,
            "phonon2_enc_set_subsampling",
            SetSubsampling
        );
        let set_layer = symbol!(encoder_library, "phonon2_enc_set_layer", SetLayer);
        let create_decoder = symbol!(decoder_library, "phonon2_tdt_create", CreateDecoder);
        let mut native = Self {
            encoder: None,
            decoder: None,
            matrices: Vec::new(),
            planes: Vec::new(),
            quantized: Vec::new(),
            half_vectors: Vec::new(),
            vectors: Vec::new(),
            durations: Box::new([0, 1, 2, 3, 4]),
            destroy_encoder: symbol!(encoder_library, "phonon2_enc_destroy", Destroy),
            destroy_decoder: symbol!(decoder_library, "phonon2_tdt_destroy", Destroy),
            destroy_matrix: symbol!(encoder_library, "phonon2_cpu_destroy", Destroy),
            forward: symbol!(encoder_library, "phonon2_enc_forward", Forward),
            out_len: symbol!(
                encoder_library,
                "phonon2_enc_out_len",
                unsafe extern "C" fn(Handle, c_int) -> c_int
            ),
            decode: symbol!(decoder_library, "phonon2_tdt_decode", Decode),
            set_encoder_threads: symbol!(
                encoder_library,
                "phonon2_cpu_set_threads",
                unsafe extern "C" fn(c_int)
            ),
            set_decoder_threads: symbol!(
                decoder_library,
                "phonon2_tdt_set_threads",
                unsafe extern "C" fn(c_int)
            ),
        };
        THREAD_POOLS.call_once(|| native.threads());
        // SAFETY: fixed Phonon-2 architecture, validated shapes before supplying weights.
        native.encoder = Some(handle(
            unsafe { create_encoder(24, 1024, 4096, 8, 9, 128, 256) },
            "encoder",
        )?);
        let mut sub = Vec::new();
        for (layer, shape) in [
            (0, vec![256, 1, 3, 3]),
            (2, vec![256, 1, 3, 3]),
            (3, vec![256, 256, 1, 1]),
            (5, vec![256, 1, 3, 3]),
            (6, vec![256, 256, 1, 1]),
        ] {
            sub.push(native.vector(
                container,
                &format!("encoder.subsampling.layers.{layer}.weight"),
                &shape,
            )?);
            sub.push(native.vector(
                container,
                &format!("encoder.subsampling.layers.{layer}.bias"),
                &[256],
            )?);
        }
        sub.push(native.vector(
            container,
            "encoder.subsampling.linear.weight",
            &[1024, 4096],
        )?);
        sub.push(native.vector(container, "encoder.subsampling.linear.bias", &[1024])?);
        // Relative-position encoding is derived state, absent from the weight container.
        native.vectors.push(
            (0..512)
                .map(|i| 1.0 / 10000.0f32.powf((2 * i) as f32 / 1024.0))
                .collect(),
        );
        sub.push(
            native
                .vectors
                .last()
                .ok_or_else(|| Error::new("position frequencies missing"))?
                .as_ptr()
                .cast(),
        );
        let enc = native
            .encoder
            .ok_or_else(|| Error::new("encoder missing"))?
            .as_ptr();
        // SAFETY: all thirteen arrays have the fixed ABI shapes and retained storage.
        check(
            unsafe {
                set_subsampling(
                    enc, sub[0], sub[1], sub[2], sub[3], sub[4], sub[5], sub[6], sub[7], sub[8],
                    sub[9], sub[10], sub[11], sub[12],
                )
            },
            "encoder subsampling",
        )?;
        let modules = [
            ("feed_forward1.linear1", 4096, 1024),
            ("feed_forward1.linear2", 1024, 4096),
            ("self_attn.q_proj", 1024, 1024),
            ("self_attn.k_proj", 1024, 1024),
            ("self_attn.v_proj", 1024, 1024),
            ("self_attn.o_proj", 1024, 1024),
            ("self_attn.relative_k_proj", 1024, 1024),
            ("conv.pointwise_conv1", 2048, 1024),
            ("conv.pointwise_conv2", 1024, 1024),
            ("feed_forward2.linear1", 4096, 1024),
            ("feed_forward2.linear2", 1024, 4096),
        ];
        for layer in 0..24 {
            let prefix = format!("encoder.layers.{layer}");
            let mut handles = Vec::new();
            for (name, rows, cols) in modules {
                let planes = container.planes(&format!("{prefix}.{name}"), rows, cols)?;
                let h = handle(
                    unsafe {
                        create_matrix(
                            rows as c_int,
                            cols as c_int,
                            planes.a.as_ptr().cast(),
                            planes.b.as_ptr().cast(),
                            planes.lo.as_ptr().cast(),
                            planes.hi.as_ptr().cast(),
                            0,
                        )
                    },
                    "packed matrix",
                )?;
                native.matrices.push(h);
                native.planes.push(planes);
                unsafe {
                    enable_onedot(h.as_ptr());
                }
                handles.push(h.as_ptr());
            }
            let mut vectors = Vec::new();
            for norm in [
                "norm_feed_forward1",
                "norm_self_att",
                "norm_conv",
                "norm_feed_forward2",
                "norm_out",
            ] {
                for suffix in ["weight", "bias"] {
                    vectors.push(native.vector(
                        container,
                        &format!("{prefix}.{norm}.{suffix}"),
                        &[1024],
                    )?);
                }
            }
            for bias in ["bias_u", "bias_v"] {
                vectors.push(native.vector(
                    container,
                    &format!("{prefix}.self_attn.{bias}"),
                    &[8, 128],
                )?);
            }
            vectors.push(native.vector(
                container,
                &format!("{prefix}.conv.depthwise_conv.weight"),
                &[1024, 1, 9],
            )?);
            for suffix in ["weight", "bias", "running_mean", "running_var"] {
                vectors.push(native.vector(
                    container,
                    &format!("{prefix}.conv.norm.{suffix}"),
                    &[1024],
                )?);
            }
            // SAFETY: eleven live matrix handles and seventeen shape-checked vectors,
            // in exactly the upstream binding's order. C copies the vectors.
            check(
                unsafe { set_layer(enc, layer, handles.as_ptr().cast(), vectors.as_ptr().cast()) },
                "encoder layer",
            )?;
        }
        let mut decoder = Vec::new();
        for (name, shape, biases) in [
            ("decoder.embedding", vec![8193, 640], vec![]),
            (
                "decoder.lstm.weight_ih_l0",
                vec![2560, 640],
                vec!["decoder.lstm.bias_ih_l0"],
            ),
            (
                "decoder.lstm.weight_hh_l0",
                vec![2560, 640],
                vec!["decoder.lstm.bias_hh_l0"],
            ),
            (
                "decoder.lstm.weight_ih_l1",
                vec![2560, 640],
                vec!["decoder.lstm.bias_ih_l1"],
            ),
            (
                "decoder.lstm.weight_hh_l1",
                vec![2560, 640],
                vec!["decoder.lstm.bias_hh_l1"],
            ),
            (
                "decoder.decoder_projector",
                vec![640, 640],
                vec!["decoder.decoder_projector.bias"],
            ),
            ("joint.head", vec![8198, 640], vec!["joint.head.bias"]),
        ] {
            let weight = if name.contains("lstm") {
                name.to_string()
            } else {
                format!("{name}.weight")
            };
            let quantized = container.quantized(&weight, &shape)?;
            decoder.push(quantized.values.as_ptr().cast());
            decoder.push(quantized.scales.as_ptr().cast());
            native.quantized.push(quantized);
            for bias in biases {
                let v = container.half(bias, &[shape[0]])?;
                decoder.push(v.as_ptr().cast());
                native.half_vectors.push(v);
            }
        }
        // SAFETY: twenty retained decoder arrays in upstream ctypes order, including
        // int8 tables and aligned FP16 scale/bias arrays. Durations remain owned.
        native.decoder = Some(handle(
            unsafe {
                create_decoder(
                    8193,
                    640,
                    640,
                    8198,
                    5,
                    native.durations.as_ptr().cast(),
                    8192,
                    10,
                    decoder[0],
                    decoder[1],
                    decoder[2],
                    decoder[3],
                    decoder[4],
                    decoder[5],
                    decoder[6],
                    decoder[7],
                    decoder[8],
                    decoder[9],
                    decoder[10],
                    decoder[11],
                    decoder[12],
                    decoder[13],
                    decoder[14],
                    decoder[15],
                    decoder[16],
                    decoder[17],
                    decoder[18],
                    decoder[19],
                )
            },
            "TDT decoder",
        )?);
        Ok(native)
    }

    fn vector(&mut self, container: &Container, name: &str, shape: &[usize]) -> Result<Ptr> {
        let vector = container.dense(name, shape)?;
        let ptr = vector.as_ptr().cast();
        self.vectors.push(vector);
        Ok(ptr)
    }

    fn threads(&self) {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(1) as c_int;
        unsafe {
            (self.set_encoder_threads)(threads);
            (self.set_decoder_threads)(threads);
        }
    }

    pub fn encode(&self, values: &[f32], frames: usize) -> Result<Vec<f32>> {
        let handle = self
            .encoder
            .ok_or_else(|| Error::new("encoder missing"))?
            .as_ptr();
        let length = unsafe { (self.out_len)(handle, frames as c_int) };
        if length <= 0 || length as usize > frames {
            return Err(Error::new("invalid native encoder output length"));
        }
        let mut output = vec![0.0; length as usize * 1024];
        // SAFETY: input is frames*128; output was sized through this encoder's out_len.
        check(
            unsafe {
                (self.forward)(
                    handle,
                    values.as_ptr().cast(),
                    frames as c_int,
                    output.as_mut_ptr().cast(),
                )
            },
            "encoder forward",
        )?;
        Ok(output)
    }

    pub fn decode(&self, projected: &[f32]) -> Result<Vec<i32>> {
        let handle = self
            .decoder
            .ok_or_else(|| Error::new("decoder missing"))?
            .as_ptr();
        let frames = projected.len() / 640;
        let mut tokens = vec![0; (frames * 12).max(64)];
        // SAFETY: contiguous frames*640 input and token buffer with explicit capacity.
        let count = unsafe {
            (self.decode)(
                handle,
                projected.as_ptr().cast(),
                frames as c_int,
                tokens.as_mut_ptr().cast(),
                tokens.len() as c_int,
            )
        };
        if count < 0 || count as usize > tokens.len() {
            return Err(Error::new(format!("TDT decode failed: {count}")));
        }
        tokens.truncate(count as usize);
        Ok(tokens)
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // SAFETY: destruction occurs under NATIVE_LOCK; libraries and buffers are
        // still alive. Destroy the encoder before the packed handles it references.
        unsafe {
            if let Some(h) = self.encoder.take() {
                (self.destroy_encoder)(h.as_ptr());
            }
            if let Some(h) = self.decoder.take() {
                (self.destroy_decoder)(h.as_ptr());
            }
            for h in self.matrices.drain(..) {
                (self.destroy_matrix)(h.as_ptr());
            }
        }
    }
}

fn handle(ptr: Handle, name: &str) -> Result<NonNull<c_void>> {
    NonNull::new(ptr).ok_or_else(|| Error::new(format!("failed to create native {name}")))
}

fn check(code: c_int, operation: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::new(format!("{operation} failed: {code}")))
    }
}

fn verified_library(
    directory: &Path,
    name: &str,
    hash: &str,
    slot: &'static OnceLock<Library>,
) -> Result<&'static Library> {
    let path = directory.join(name);
    let bytes = std::fs::read(&path).map_err(|e| Error::path(&path, e))?;
    if format!("{:x}", Sha256::digest(&bytes)) != hash {
        return Err(Error::new(format!(
            "native library checksum mismatch: {}",
            path.display()
        )));
    }
    if let Some(library) = slot.get() {
        return Ok(library);
    }
    // SAFETY: only the pinned upstream binary selected for this host may execute.
    // The process-wide slot keeps code alive for the upstream worker threads.
    let library = unsafe { Library::new(&path)? };
    Ok(slot.get_or_init(|| library))
}

fn library_names() -> Result<(&'static str, &'static str, &'static str, &'static str)> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        if !std::is_x86_feature_detected!("sse4.1") {
            return Err(Error::new("Phonon-2 requires SSE4.1 on Linux x86_64"));
        }
        let (enc, pin) =
            if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
                (
                    "libphonon2_enc-linux-x86_64.so",
                    "8b0beb8c7dc4e4239b4f3348b28750530739711e9204a411e8ff8eceeabe20a3",
                )
            } else {
                (
                    "libphonon2_enc-linux-x86_64-base.so",
                    "c5eca3010708249edb4165356689e6b93dcaed75dc27c24f0b4b7471f6025b81",
                )
            };
        Ok((
            enc,
            pin,
            "libphonon2_tdt-linux-x86_64.so",
            "3c0bea612b60c053865fbb6f315b39b481bc37f0429fa4608cdaed4d11d1b468",
        ))
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        if std::arch::is_aarch64_feature_detected!("dotprod")
            && std::arch::is_aarch64_feature_detected!("lse")
        {
            return Ok((
                "libphonon2_enc-linux-aarch64.so",
                "084de96e3f623fb1b35d390b54d57db67f1c2ab8c6abecdbd48d261ac58c5f08",
                "libphonon2_tdt-linux-aarch64.so",
                "edb874d62311169dc305e3604fd3595813c07a7af5045fa67385e9c57838dbe5",
            ));
        }
        Ok((
            "libphonon2_enc-linux-aarch64-base.so",
            "72c9d1916f32f00fddd172126cfaa2e51ae4aaaa391e96fbcb716b12dd856758",
            "libphonon2_tdt-linux-aarch64-base.so",
            "b48179d39bb35ffc51169c3bed4845b2959217cfd02dbf74cf9abe9e21cd1b72",
        ))
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    Err(Error::new(
        "Phonon-2 native integration supports Linux x86_64 and aarch64",
    ))
}
