//! Hardware video decoding and encoding through libavcodec. There is deliberately no
//! software fallback for video: without a usable GPU codec, nicedelay says so instead.

use std::collections::HashMap;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use ffmpeg_next::codec::{self, Parameters};
use ffmpeg_next::ffi::{self, AVHWDeviceType, AVPixelFormat};
use ffmpeg_next::{Rational, decoder, frame};

/// Hardware device types to try for decoding, best first.
fn device_types() -> &'static [AVHWDeviceType] {
    if cfg!(target_os = "macos") {
        &[AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX]
    } else if cfg!(windows) {
        &[
            AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA,
            AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
        ]
    } else {
        &[AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI, AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA]
    }
}

/// Hardware H.264 encoders to try, best first, on this platform.
pub fn encoder_candidates() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["h264_videotoolbox"]
    } else if cfg!(windows) {
        &["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"]
    } else {
        &["h264_nvenc", "h264_vaapi", "h264_qsv"]
    }
}

/// Encoder settings for live streaming: constant bitrate, low latency, no B-frames, and
/// never the vendor's software fallback.
pub fn encoder_options(name: &str) -> Vec<(&'static str, &'static str)> {
    match name {
        "h264_videotoolbox" => {
            vec![("realtime", "1"), ("allow_sw", "0"), ("profile", "high"), ("constant_bit_rate", "0")]
        }
        "h264_nvenc" => {
            vec![("preset", "p4"), ("tune", "ll"), ("rc", "cbr"), ("profile", "high"), ("zerolatency", "1")]
        }
        "h264_amf" => vec![("usage", "lowlatency"), ("rc", "cbr"), ("quality", "speed"), ("profile", "high")],
        "h264_qsv" => vec![("preset", "veryfast"), ("profile", "high"), ("look_ahead", "0")],
        "h264_vaapi" => vec![("rc_mode", "CBR"), ("profile", "high")],
        "h264_mf" => vec![("hw_encoding", "1"), ("rate_control", "cbr"), ("scenario", "live_streaming")],
        _ => vec![],
    }
}

/// VAAPI encoders only take pictures already uploaded to the GPU.
pub fn encoder_device(name: &str) -> Option<AVHWDeviceType> {
    (name == "h264_vaapi").then_some(AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)
}

/// A reference to an FFmpeg hardware device context.
pub struct Device(*mut ffi::AVBufferRef);

// Hardware device contexts are reference counted and thread-safe in FFmpeg.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    pub fn as_ptr(&self) -> *mut ffi::AVBufferRef {
        self.0
    }

    /// A new reference for a codec context to own.
    pub fn new_ref(&self) -> *mut ffi::AVBufferRef {
        unsafe { ffi::av_buffer_ref(self.0) }
    }
}

impl Clone for Device {
    fn clone(&self) -> Self {
        Device(self.new_ref())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

pub fn device_name(kind: AVHWDeviceType) -> String {
    unsafe {
        let name = ffi::av_hwdevice_get_type_name(kind);
        if name.is_null() { "unknown".into() } else { std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned() }
    }
}

/// The shared device of a type, created on first use (one per process is enough).
pub fn device(kind: AVHWDeviceType) -> Result<Device, String> {
    static DEVICES: OnceLock<Mutex<HashMap<i32, Device>>> = OnceLock::new();
    let mut devices = DEVICES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = devices.get(&(kind as i32)) {
        return Ok(d.clone());
    }
    let mut ptr = ptr::null_mut();
    let r = unsafe { ffi::av_hwdevice_ctx_create(&mut ptr, kind, ptr::null(), ptr::null_mut(), 0) };
    if r < 0 || ptr.is_null() {
        return Err(format!("{} isn't available ({})", device_name(kind), ffmpeg_next::Error::from(r)));
    }
    let d = Device(ptr);
    devices.insert(kind as i32, d.clone());
    Ok(d)
}

/// The pixel format a decoder produces on a device type, if it can use that device.
fn hw_pixel_format(codec: &ffmpeg_next::codec::codec::Codec, kind: AVHWDeviceType) -> Option<AVPixelFormat> {
    const METHOD_HW_DEVICE_CTX: i32 = 0x01; // AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX
    for i in 0.. {
        let cfg = unsafe { ffi::avcodec_get_hw_config(codec.as_ptr(), i) };
        if cfg.is_null() {
            return None;
        }
        let cfg = unsafe { &*cfg };
        if cfg.methods & METHOD_HW_DEVICE_CTX != 0 && cfg.device_type == kind {
            return Some(cfg.pix_fmt);
        }
    }
    None
}

/// Picks the hardware format libavcodec offers; anything else would mean software decoding.
unsafe extern "C" fn get_hw_format(ctx: *mut ffi::AVCodecContext, formats: *const AVPixelFormat) -> AVPixelFormat {
    unsafe {
        let want = (*ctx).opaque as isize as i32;
        let mut p = formats;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p as i32 == want {
                return *p;
            }
            p = p.add(1);
        }
    }
    AVPixelFormat::AV_PIX_FMT_NONE
}

/// Opens a hardware decoder for a video stream. With `keyframes_only`, everything but
/// keyframes is skipped (cheap thumbnails).
pub fn open_video_decoder(
    params: &Parameters,
    time_base: Rational,
    keyframes_only: bool,
) -> Result<(decoder::Video, String), String> {
    let codec = decoder::find(params.id()).ok_or_else(|| format!("no decoder for {:?}", params.id()))?;
    let mut errors = vec![];
    for &kind in device_types() {
        let Some(pix) = hw_pixel_format(&codec, kind) else {
            errors.push(format!("{} can't decode {}", device_name(kind), codec.name()));
            continue;
        };
        let dev = match device(kind) {
            Ok(d) => d,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let mut ctx = codec::context::Context::from_parameters(params.clone()).map_err(|e| e.to_string())?;
        unsafe {
            let c = ctx.as_mut_ptr();
            (*c).hw_device_ctx = dev.new_ref();
            (*c).opaque = pix as i32 as isize as *mut std::ffi::c_void;
            (*c).get_format = Some(get_hw_format);
        }
        let mut dec = ctx.decoder();
        dec.set_packet_time_base(time_base);
        if keyframes_only {
            dec.skip_frame(ffmpeg_next::codec::discard::Discard::NonKey);
        }
        match dec.open_as(codec).and_then(|o| o.video()) {
            Ok(v) => return Ok((v, device_name(kind))),
            Err(e) => errors.push(format!("{}: {e}", device_name(kind))),
        }
    }
    Err(format!("no hardware decoder for {} ({})", codec.name(), errors.join("; ")))
}

/// Copies a hardware-decoded frame into system memory (NV12, or P010 for 10-bit video).
pub fn download(hw: &frame::Video) -> Result<frame::Video, String> {
    unsafe {
        if (*hw.as_ptr()).hw_frames_ctx.is_null() {
            return Ok(hw.clone());
        }
        let mut sw = frame::Video::empty();
        let r = ffi::av_hwframe_transfer_data(sw.as_mut_ptr(), hw.as_ptr(), 0);
        if r < 0 {
            return Err(format!("couldn't copy a decoded frame from the GPU: {}", ffmpeg_next::Error::from(r)));
        }
        Ok(sw)
    }
}

/// Frames for a hardware encoder that needs GPU pictures (VAAPI).
pub struct FramePool(*mut ffi::AVBufferRef);

unsafe impl Send for FramePool {}

impl FramePool {
    pub fn new(device: &Device, w: u32, h: u32) -> Result<FramePool, String> {
        unsafe {
            let mut r = ffi::av_hwframe_ctx_alloc(device.as_ptr());
            if r.is_null() {
                return Err("couldn't create GPU frames".into());
            }
            let fc = (*r).data as *mut ffi::AVHWFramesContext;
            (*fc).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*fc).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*fc).width = w as i32;
            (*fc).height = h as i32;
            (*fc).initial_pool_size = 20;
            let e = ffi::av_hwframe_ctx_init(r);
            if e < 0 {
                ffi::av_buffer_unref(&mut r);
                return Err(format!("couldn't create GPU frames: {}", ffmpeg_next::Error::from(e)));
            }
            Ok(FramePool(r))
        }
    }

    pub fn new_ref(&self) -> *mut ffi::AVBufferRef {
        unsafe { ffi::av_buffer_ref(self.0) }
    }

    /// Uploads a frame in system memory to a GPU frame from the pool.
    pub fn upload(&self, sw: &frame::Video) -> Result<frame::Video, String> {
        unsafe {
            let mut hw = frame::Video::empty();
            let e = ffi::av_hwframe_get_buffer(self.0, hw.as_mut_ptr(), 0);
            if e < 0 {
                return Err(format!("no GPU frame available: {}", ffmpeg_next::Error::from(e)));
            }
            let e = ffi::av_hwframe_transfer_data(hw.as_mut_ptr(), sw.as_ptr(), 0);
            if e < 0 {
                return Err(format!("couldn't upload a frame to the GPU: {}", ffmpeg_next::Error::from(e)));
            }
            (*hw.as_mut_ptr()).pts = (*sw.as_ptr()).pts;
            Ok(hw)
        }
    }
}

impl Drop for FramePool {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}
