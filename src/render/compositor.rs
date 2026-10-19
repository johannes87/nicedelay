//! Draws with Skia on NV12 pictures: the part to draw on is converted to RGBA with
//! libswscale, drawn on, and converted back. The badge only touches its own small
//! rectangle; transitions redraw the whole frame.

use std::sync::Arc;

use skia_safe::{AlphaType, Canvas, ColorType, Data, Image, ImageInfo, Surface, images, surfaces};

use crate::config::BadgePosition;
use crate::media::picture::{Picture, Sws};
use crate::render::overlay::{Overlay, PillParams};

pub struct Compositor {
    pub w: u32,
    pub h: u32,
    to_rgba: Sws,
    to_yuv: Sws,
    region: Vec<u8>,
    frame: Surface,
    /// the last picture turned into an image, so repeats don't convert it again
    cache: Vec<(Arc<Vec<u8>>, Image)>,
}

// Skia raster surfaces and images are plain memory; the compositor is used by one thread
// at a time (the playout's).
unsafe impl Send for Compositor {}

fn info(w: u32, h: u32) -> ImageInfo {
    ImageInfo::new((w as i32, h as i32), ColorType::RGBA8888, AlphaType::Premul, None)
}

impl Compositor {
    pub fn new(w: u32, h: u32) -> Compositor {
        let frame = surfaces::raster(&info(w, h), None, None).expect("can't allocate a frame-sized canvas");
        Compositor { w, h, to_rgba: Sws::default(), to_yuv: Sws::default(), region: vec![], frame, cache: vec![] }
    }

    /// Draws the badge onto a picture, touching only the pixels under it.
    pub fn pill(&mut self, pic: &mut Picture, overlay: &Overlay, p: &PillParams, pos: BadgePosition) {
        let (layout, text) = overlay.pill_layout(p, pos);
        if layout.w < 2 || layout.h < 2 {
            return;
        }
        let bytes = (layout.w * layout.h * 4) as usize;
        self.region.resize(bytes, 0);
        self.to_rgba.region_to_rgba(pic, layout.x, layout.y, layout.w, layout.h, &mut self.region);
        if let Some(mut surface) = surfaces::wrap_pixels(&info(layout.w, layout.h), &mut self.region, None, None) {
            overlay.draw_pill(surface.canvas(), p, &layout, &text);
        }
        self.to_yuv.rgba_to_region(&self.region, layout.w, layout.h, pic, layout.x, layout.y);
    }

    /// A picture as a Skia image (converted once, then reused while it repeats).
    pub fn image(&mut self, pic: &Picture) -> Option<Image> {
        if let Some((_, img)) = self.cache.iter().find(|(d, _)| Arc::ptr_eq(d, &pic.data)) {
            return Some(img.clone());
        }
        let mut rgba = vec![0u8; (pic.w * pic.h * 4) as usize];
        self.to_rgba.region_to_rgba(pic, 0, 0, pic.w, pic.h, &mut rgba);
        let img = images::raster_from_data(&info(pic.w, pic.h), Data::new_copy(&rgba), (pic.w * 4) as usize)?;
        self.cache.push((pic.data.clone(), img.clone()));
        if self.cache.len() > 4 {
            self.cache.remove(0);
        }
        Some(img)
    }

    /// The frame-sized canvas for drawing a whole transition frame.
    pub fn canvas(&mut self) -> &Canvas {
        self.frame.canvas()
    }

    /// What's on the frame canvas right now.
    pub fn snapshot(&mut self) -> Image {
        self.frame.image_snapshot()
    }

    /// The frame canvas as a new picture.
    pub fn finish(&mut self) -> Picture {
        let mut pic = Picture::black(self.w, self.h);
        if let Some(pixmap) = self.frame.peek_pixels()
            && let Some(rgba) = pixmap.bytes()
        {
            self.to_yuv.rgba_to_region(rgba, self.w, self.h, &mut pic, 0, 0);
        }
        pic
    }
}
