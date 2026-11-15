//! Album art in the terminal: color half-blocks or monochrome ASCII.

use crate::config::CoverMode;
use anyhow::Result;
use image::imageops::FilterType;
use image::RgbImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

pub fn decode(bytes: &[u8]) -> Result<RgbImage> {
    Ok(image::load_from_memory(bytes)?.to_rgb8())
}

/// Characters from dark to bright (dense characters are bright on a dark terminal).
const RAMP: &[u8] = b" .:-=+*#%@";

pub struct Cover<'a> {
    pub image: &'a RgbImage,
    pub mode: CoverMode,
}

impl Widget for Cover<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        match self.mode {
            CoverMode::Off => {}
            CoverMode::Blocks => {
                // Each cell shows two pixels: the upper as foreground of "▀", the lower as background.
                let img = image::imageops::resize(self.image, area.width as u32, area.height as u32 * 2, FilterType::Triangle);
                for y in 0..area.height {
                    for x in 0..area.width {
                        let top = img.get_pixel(x as u32, y as u32 * 2).0;
                        let bottom = img.get_pixel(x as u32, y as u32 * 2 + 1).0;
                        if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                            cell.set_char('▀')
                                .set_fg(Color::Rgb(top[0], top[1], top[2]))
                                .set_bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
                        }
                    }
                }
            }
            CoverMode::Ascii => {
                let img = image::imageops::resize(self.image, area.width as u32, area.height as u32, FilterType::Triangle);
                let lum = |p: &image::Rgb<u8>| 0.2126 * p.0[0] as f32 + 0.7152 * p.0[1] as f32 + 0.0722 * p.0[2] as f32;
                // Stretch contrast per image, so mostly-light or mostly-dark covers still show shapes.
                let (lo, hi) = img.pixels().map(lum).fold((255f32, 0f32), |(lo, hi), l| (lo.min(l), hi.max(l)));
                let range = (hi - lo).max(1.0);
                for y in 0..area.height {
                    for x in 0..area.width {
                        let l = (lum(img.get_pixel(x as u32, y as u32)) - lo) / range;
                        let i = (l * (RAMP.len() - 1) as f32).round() as usize;
                        if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                            cell.set_char(RAMP[i.min(RAMP.len() - 1)] as char).set_fg(Color::Gray);
                        }
                    }
                }
            }
        }
    }
}
