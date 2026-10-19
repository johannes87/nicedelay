//! Text for the on-stream graphics, set in the bundled Inter font so it looks the same on
//! every computer.

use skia_safe::textlayout::{
    FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle, TextAlign, TextShadow, TextStyle, TypefaceFontProvider,
};
use skia_safe::{Color, Data, FontMgr, FontStyle, Point, font_style};

pub const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
pub const INTER_BOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-Bold.ttf");

const FAMILY: &str = "Inter";

pub struct Fonts {
    collection: FontCollection,
}

pub struct Style {
    pub size: f32,
    pub weight: i32,
    pub color: Color,
    pub letter_spacing: f32,
    /// shadow colour and blur (canvas-style `shadowBlur`, twice the sigma)
    pub shadow: Option<(Color, f32)>,
}

impl Fonts {
    pub fn new() -> Fonts {
        let mgr = FontMgr::new();
        let mut provider = TypefaceFontProvider::new();
        for data in [INTER_REGULAR, INTER_SEMIBOLD, INTER_BOLD] {
            if let Some(tf) = mgr.new_from_data(Data::new_copy(data), None) {
                provider.register_typeface(tf, Some(FAMILY));
            }
        }
        let mut collection = FontCollection::new();
        // Skia only uses the "default" manager for fallback fonts; ours is an asset font.
        collection.set_asset_font_manager(Some(provider.into()));
        collection.disable_font_fallback();
        Fonts { collection }
    }

    /// One line of text, laid out `width` wide with the given alignment.
    pub fn line(&self, text: &str, style: &Style, align: TextAlign, width: f32) -> Paragraph {
        let mut ts = TextStyle::new();
        ts.set_font_families(&[FAMILY]);
        ts.set_font_size(style.size);
        ts.set_font_style(FontStyle::new(
            font_style::Weight::from(style.weight),
            font_style::Width::NORMAL,
            font_style::Slant::Upright,
        ));
        ts.set_color(style.color);
        ts.set_letter_spacing(style.letter_spacing);
        if let Some((color, blur)) = style.shadow {
            ts.add_shadow(TextShadow::new(color, Point::new(0.0, 0.0), f64::from(blur / 2.0)));
        }
        let mut ps = ParagraphStyle::new();
        ps.set_text_align(align);
        ps.set_max_lines(1);
        let mut b = ParagraphBuilder::new(&ps, &self.collection);
        b.push_style(&ts);
        b.add_text(text);
        let mut p = b.build();
        p.layout(width);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_text_in_inter() {
        let fonts = Fonts::new();
        let style = Style { size: 22.0, weight: 600, color: Color::WHITE, letter_spacing: 1.0, shadow: None };
        let p = fonts.line("LIVE", &style, TextAlign::Left, 1000.0);
        assert!(p.max_intrinsic_width() > 20.0, "width {}", p.max_intrinsic_width());
    }
}
