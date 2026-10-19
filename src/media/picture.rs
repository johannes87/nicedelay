//! Raw pictures (NV12, as hardware decoders and encoders want them) and libswscale glue.

use std::ptr;
use std::sync::Arc;

use ffmpeg_next::ffi;
use ffmpeg_next::ffi::AVPixelFormat;

/// An NV12 picture: a full-size luma plane, then interleaved half-size chroma (U, V).
/// The pixels are shared: repeating a frame doesn't copy it, drawing on one does.
#[derive(Clone)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub data: Arc<Vec<u8>>,
}

impl Picture {
    pub fn filled(w: u32, h: u32, luma: u8, chroma: u8) -> Self {
        let ys = (w * h) as usize;
        let mut data = vec![chroma; ys * 3 / 2];
        data[..ys].fill(luma);
        Picture { w, h, data: Arc::new(data) }
    }

    pub fn black(w: u32, h: u32) -> Self {
        Self::filled(w, h, 16, 128)
    }

    #[cfg(test)]
    pub fn y(&self) -> &[u8] {
        &self.data[..(self.w * self.h) as usize]
    }

    /// Pixels to draw on, copied first if anyone else holds them.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        Arc::make_mut(&mut self.data).as_mut_slice()
    }

    pub fn same_pixels(&self, other: &Picture) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }
}

/// Plane pointers and strides of a rectangle inside an NV12 picture (x, y even).
fn nv12_planes(pic: *mut u8, w: u32, h: u32, x: u32, y: u32) -> ([*mut u8; 4], [i32; 4]) {
    let stride = w as usize;
    unsafe {
        let luma = pic.add(y as usize * stride + x as usize);
        let chroma = pic.add(stride * h as usize + (y / 2) as usize * stride + x as usize);
        ([luma, chroma, ptr::null_mut(), ptr::null_mut()], [w as i32, w as i32, 0, 0])
    }
}

/// A libswscale context, reconfigured only when sizes or formats change.
pub struct Sws {
    ctx: *mut ffi::SwsContext,
    key: (AVPixelFormat, u32, u32, AVPixelFormat, u32, u32),
}

// A SwsContext belongs to whoever holds it; it's only ever used by one thread at a time.
unsafe impl Send for Sws {}

pub const SWS_BILINEAR: i32 = 2;
pub const SWS_BICUBIC: i32 = 4;
pub const SWS_AREA: i32 = 0x20;

impl Default for Sws {
    fn default() -> Self {
        Sws { ctx: ptr::null_mut(), key: (AVPixelFormat::AV_PIX_FMT_NONE, 0, 0, AVPixelFormat::AV_PIX_FMT_NONE, 0, 0) }
    }
}

fn is_rgb(f: AVPixelFormat) -> bool {
    matches!(f, AVPixelFormat::AV_PIX_FMT_RGBA | AVPixelFormat::AV_PIX_FMT_BGRA)
}

impl Sws {
    /// Prepares a conversion; YUV is BT.709 limited range, RGB is full range.
    fn prepare(&mut self, src: (AVPixelFormat, u32, u32), dst: (AVPixelFormat, u32, u32), flags: i32) -> bool {
        let key = (src.0, src.1, src.2, dst.0, dst.1, dst.2);
        if !self.ctx.is_null() && self.key == key {
            return true;
        }
        unsafe {
            self.ctx = ffi::sws_getCachedContext(
                self.ctx,
                src.1 as i32,
                src.2 as i32,
                src.0,
                dst.1 as i32,
                dst.2 as i32,
                dst.0,
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            if self.ctx.is_null() {
                return false;
            }
            let bt709 = ffi::sws_getCoefficients(1); // SWS_CS_ITU709
            let src_full = i32::from(is_rgb(src.0));
            let dst_full = i32::from(is_rgb(dst.0));
            ffi::sws_setColorspaceDetails(self.ctx, bt709, src_full, bt709, dst_full, 0, 1 << 16, 1 << 16);
        }
        self.key = key;
        true
    }

    fn run(&mut self, src: ([*const u8; 4], [i32; 4]), src_h: u32, dst: ([*mut u8; 4], [i32; 4])) {
        unsafe {
            ffi::sws_scale(self.ctx, src.0.as_ptr(), src.1.as_ptr(), 0, src_h as i32, dst.0.as_ptr(), dst.1.as_ptr());
        }
    }

    /// Scales a decoded frame into a new `w`×`h` picture, letterboxed to keep its aspect
    /// ratio (square pixels).
    pub fn fit_frame(&mut self, frame: &ffi::AVFrame, w: u32, h: u32, flags: i32) -> Option<Picture> {
        let (fw, fh) = (frame.width as u32, frame.height as u32);
        if fw == 0 || fh == 0 {
            return None;
        }
        let scale = f64::min(f64::from(w) / f64::from(fw), f64::from(h) / f64::from(fh));
        let sw = ((f64::from(fw) * scale / 2.0).round() as u32 * 2).clamp(2, w);
        let sh = ((f64::from(fh) * scale / 2.0).round() as u32 * 2).clamp(2, h);
        let (x, y) = (((w - sw) / 4) * 2, ((h - sh) / 4) * 2);
        let format: AVPixelFormat = unsafe { std::mem::transmute(frame.format) };
        if !self.prepare((format, fw, fh), (AVPixelFormat::AV_PIX_FMT_NV12, sw, sh), flags) {
            return None;
        }
        let mut pic = Picture::black(w, h);
        let dst = nv12_planes(pic.pixels_mut().as_mut_ptr(), w, h, x, y);
        let src = ([0, 1, 2, 3].map(|i| frame.data[i].cast_const()), [0, 1, 2, 3].map(|i| frame.linesize[i]));
        self.run(src, fh, dst);
        Some(pic)
    }

    /// Converts a rectangle of an NV12 picture to RGBA (`rgba` holds rw*rh*4 bytes).
    pub fn region_to_rgba(&mut self, pic: &Picture, x: u32, y: u32, rw: u32, rh: u32, rgba: &mut [u8]) {
        if !self.prepare(
            (AVPixelFormat::AV_PIX_FMT_NV12, rw, rh),
            (AVPixelFormat::AV_PIX_FMT_RGBA, rw, rh),
            SWS_BILINEAR,
        ) {
            return;
        }
        let (planes, strides) = nv12_planes(pic.data.as_ptr().cast_mut(), pic.w, pic.h, x, y);
        let src = (planes.map(|p| p.cast_const()), strides);
        let dst = ([rgba.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()], [rw as i32 * 4, 0, 0, 0]);
        self.run(src, rh, dst);
    }

    /// Writes an RGBA image back into a rectangle of an NV12 picture.
    pub fn rgba_to_region(&mut self, rgba: &[u8], rw: u32, rh: u32, pic: &mut Picture, x: u32, y: u32) {
        if !self.prepare(
            (AVPixelFormat::AV_PIX_FMT_RGBA, rw, rh),
            (AVPixelFormat::AV_PIX_FMT_NV12, rw, rh),
            SWS_BILINEAR,
        ) {
            return;
        }
        let (w, h) = (pic.w, pic.h);
        let dst = nv12_planes(pic.pixels_mut().as_mut_ptr(), w, h, x, y);
        let src = ([rgba.as_ptr(), ptr::null(), ptr::null(), ptr::null()], [rw as i32 * 4, 0, 0, 0]);
        self.run(src, rh, dst);
    }

    /// Scales a whole NV12 picture to an RGBA image of another size (e.g. the preview).
    pub fn picture_to_rgba(&mut self, pic: &Picture, w: u32, h: u32, rgba: &mut [u8]) {
        if !self.prepare(
            (AVPixelFormat::AV_PIX_FMT_NV12, pic.w, pic.h),
            (AVPixelFormat::AV_PIX_FMT_RGBA, w, h),
            SWS_BILINEAR,
        ) {
            return;
        }
        let (planes, strides) = nv12_planes(pic.data.as_ptr().cast_mut(), pic.w, pic.h, 0, 0);
        let src = (planes.map(|p| p.cast_const()), strides);
        let dst = ([rgba.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()], [w as i32 * 4, 0, 0, 0]);
        self.run(src, pic.h, dst);
    }
}

impl Drop for Sws {
    fn drop(&mut self) {
        unsafe { ffi::sws_freeContext(self.ctx) };
    }
}
