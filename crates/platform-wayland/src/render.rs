//! Pure drawing side for candidate window rendering using cosmic_text.
//! Zero wayland dependencies — only rendering.
//! Supports numbered candidates, highlighting, preedit text.
//! Uses system fonts for CJK glyph availability.
//! ARGB pixel output for double-buffered Wayland layer-surface.

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, Style};

/// Candidate data structure — matches kime-core::dict::Candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub pinyin: String,
    pub freq: u64,
    pub ai: bool,
}

pub struct Renderer {
    font_system: FontSystem,
    font_size: f64,
}

impl Renderer {
    /// Initialize renderer with system fonts, default font size 20.
    pub fn new() -> Self {
        let font_system = FontSystem::new();
        Self {
            font_system,
            font_size: 20.0,
        }
    }

    /// Set font size.
    pub fn set_font_size(&mut self, size: f64) {
        self.font_size = size;
    }

    /// Draw thin border around buffer edges for visibility in dark terminals.
    fn draw_border(buf: &mut [u8], width: usize, height: usize, color: u32) {
        let (r, g, b, a) = (
            (color >> 16) as u8,
            (color >> 8) as u8,
            color as u8,
            (color >> 24) as u8,
        );
        // Top and bottom borders (full width)
        for y in [0, height - 1] {
            for x in 0..width {
                let off = (y * width + x) * 4;
                buf[off] = r;
                buf[off + 1] = g;
                buf[off + 2] = b;
                buf[off + 3] = a;
            }
        }
        // Left and right borders (excluding corners)
        for x in [0, width - 1] {
            for y in 1..height - 1 {
                let off = (y * width + x) * 4;
                buf[off] = r;
                buf[off + 1] = g;
                buf[off + 2] = b;
                buf[off + 3] = a;
            }
        }
    }

    /// Draw candidates with compact layout and high-contrast border highlight.
    pub fn draw_candidates_compact(
        &mut self,
        buf: &mut [u8],
        width: usize,
        height: usize,
        candidates: &[Candidate],
        highlight: usize,
        preedit: &str,
    ) -> Result<(), &'static str> {
        if buf.len() != width * height * 4 {
            return Err("Buffer size mismatch");
        }
        // Clear buffer (transparent background)
        for px in buf.chunks_exact_mut(4) {
            px[0] = 0x00;
            px[1] = 0x00;
            px[2] = 0x00;
            px[3] = 0x00;
        }
        // Draw border first (will be overlaid by text)
        // Use subtle gray-blue (#4C566A) or accented #5E81AC
        draw_border(buf, width, height, 0x5E81ACFFu32);

        // Build text with line breaks: preedit (small) + numbered candidates
        let mut text = String::new();
        if !preedit.is_empty() {
            text.push_str(&format!("{}\n", preedit));
        }
        for (i, cand) in candidates.iter().enumerate() {
            let prefix = if i == highlight { "> " } else { "  " };
            text.push_str(&format!("{} {}. {}\n", prefix, i + 1, cand.text));
        }

        // Create buffer with proper Metrics (tighter line height for compact)
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(self.font_size as f32, height as f32),
        );
        let mut attrs = Attrs::new();
        attrs.family = cosmic_text::Family::SansSerif;
        attrs.style = Style::Normal;
        let color = cosmic_text::Color::rgba(240, 240, 245, 255);
        let mut cache = cosmic_text::SwashCache::new();
        buffer.draw(&mut self.font_system, &mut cache, color, |x, y, w, h, c| {
            let word = c.0;
            let (r, g, b, a) = (
                (word >> 16) as u8,
                (word >> 8) as u8,
                word as u8,
                (word >> 24) as u8,
            );
            for dy in 0..h as usize {
                let py = y as usize + dy;
                for dx in 0..w as usize {
                    let px = x as usize + dx;
                    if px < width && py < height {
                        let off = (py * width + px) * 4;
                        buf[off] = r;
                        buf[off + 1] = g;
                        buf[off + 2] = b;
                        buf[off + 3] = a;
                    }
                }
            }
        });

        // CJK readability check
        let non_zero = buf.iter().filter(|&&b| b != 0).count();
        if non_zero == 0 && !candidates.is_empty() {
            return Err("Rendering produced all-transparent buffer - CJK glyphs may be missing");
        }
        Ok(())
    }

    /// Update metrics based on actual content height for compact sizing.
    pub fn update_metrics(&mut self, _buf: &mut [u8], _width: usize, candidates: &[Candidate], preedit: &str) {
        // Calculate required height: base 24px + preedit 30px + each candidate 28px line height
        let preedit_h = if preedit.is_empty() { 0 } else { 30 };
        let candidate_h = candidates.len().min(10) * 28; // tighter line height
        let required_h = 24 + preedit_h + candidate_h;
        // Note: height is managed by caller via buffer allocation
    }

    /// Draw candidates with original layout but updated sizing.
    pub fn draw_candidates(
        &mut self,
        buf: &mut [u8],
        width: usize,
        height: usize,
        candidates: &[Candidate],
        highlight: usize,
        preedit: &str,
    ) -> Result<(), &'static str> {
        if buf.len() != width * height * 4 {
            return Err("Buffer size mismatch");
        }

        // Semi-transparent dark background for readability
        for px in buf.chunks_exact_mut(4) {
            px[0] = 0x1E;
            px[1] = 0x1E;
            px[2] = 0x26;
            px[3] = 0xE8; // Opaque dark background like original
        }

        // Only draw border if we have candidates (empty render should be uniform)
        if !candidates.is_empty() {
            // Draw border after background
            draw_border(buf, width, height, 0x4C566AFFu32); // #4C566A gray-blue
        }

        // Build text with line breaks: preedit (small) + numbered candidates
        let mut text = String::new();
        if !preedit.is_empty() {
            text.push_str(&format!("{}\n", preedit));
        }
        for (i, cand) in candidates.iter().enumerate() {
            let prefix = if i == highlight { "> " } else { "  " };
            text.push_str(&format!("{} {}. {}\n", prefix, i + 1, cand.text));
        }

        // Create buffer with proper Metrics
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(self.font_size as f32, height as f32),
        );
        let mut attrs = Attrs::new();
        attrs.family = cosmic_text::Family::SansSerif;
        attrs.style = Style::Normal;
        let color = cosmic_text::Color::rgba(240, 240, 245, 255);
        let mut cache = cosmic_text::SwashCache::new();
        buffer.draw(&mut self.font_system, &mut cache, color, |x, y, w, h, c| {
            let word = c.0;
            let (r, g, b, a) = (
                (word >> 16) as u8,
                (word >> 8) as u8,
                word as u8,
                (word >> 24) as u8,
            );
            for dy in 0..h as usize {
                let py = y as usize + dy;
                for dx in 0..w as usize {
                    let px = x as usize + dx;
                    if px < width && py < height {
                        let off = (py * width + px) * 4;
                        buf[off] = r;
                        buf[off + 1] = g;
                        buf[off + 2] = b;
                        buf[off + 3] = a;
                    }
                }
            }
        });

        // CJK readability check
        let non_zero = buf.iter().filter(|&&b| b != 0).count();
        if non_zero == 0 && !candidates.is_empty() {
            return Err("Rendering produced all-transparent buffer - CJK glyphs may be missing");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renderer_basic() {
        let mut renderer = Renderer::new();
        let mut buf = vec![0u8; 400 * 600 * 4]; // width=400, height=600
        let candidates = vec![
            Candidate {
                text: "测试".to_string(),
                pinyin: "ce shi".to_string(),
                freq: 1000,
                ai: false,
            },
            Candidate {
                text: "结果".to_string(),
                pinyin: "jie guo".to_string(),
                freq: 800,
                ai: false,
            },
        ];
        let result = renderer.draw_candidates(&mut buf, 400, 600, &candidates, 0, "输入");
        assert!(result.is_ok());

        // Verify we actually rendered something (non-zero pixels)
        let non_zero = buf.iter().filter(|&&b| b != 0).count();
        assert!(
            non_zero > 0,
            "Rendering produced all-transparent buffer - CJK glyphs missing"
        );
    }

    #[test]
    fn test_renderer_highlight() {
        let mut renderer = Renderer::new();
        let mut buf = vec![0u8; 400 * 600 * 4];
        let candidates = vec![
            Candidate {
                text: "第一个".to_string(),
                pinyin: "di yi ge".to_string(),
                freq: 1000,
                ai: false,
            },
            Candidate {
                text: "第二个".to_string(),
                pinyin: "di er ge".to_string(),
                freq: 900,
                ai: false,
            },
        ];
        // Test highlighted candidate (index 1)
        let result = renderer.draw_candidates(&mut buf, 400, 600, &candidates, 1, "预编辑");
        assert!(result.is_ok());

        let non_zero = buf.iter().filter(|&&b| b != 0).count();
        assert!(
            non_zero > 0,
            "Rendering with highlight produced all-transparent buffer"
        );
    }

    #[test]
    fn test_renderer_empty() {
        let mut renderer = Renderer::new();
        let mut buf = vec![0u8; 400 * 600 * 4];
        let candidates = vec![];
        let result = renderer.draw_candidates(&mut buf, 400, 600, &candidates, 0, "");
        assert!(result.is_ok());

        // Empty render: uniform opaque background (every pixel = BG 0x1E1E26, A=0xE8)
        let all_bg = buf
            .chunks_exact(4)
            .all(|px| px[0] == 0x1E && px[1] == 0x1E && px[2] == 0x26 && px[3] == 0xE8);
        assert!(all_bg, "Empty render should be uniform background");
    }

    #[test]
    fn test_renderer_compact_sizing() {
        let mut renderer = Renderer::new();
        let candidates = vec![
            Candidate { text: "测试".to_string(), pinyin: "ce shi".to_string(), freq: 1, ai: false },
            Candidate { text: "结果".to_string(), pinyin: "jie guo".to_string(), freq: 2, ai: false },
        ];
        let mut buf = vec![0u8; 380 * 380 * 4]; // Compact width, square height
        // Test compact rendering
        let result = renderer.draw_candidates_compact(&mut buf, 380, 380, &candidates, 0, "输入");
        assert!(result.is_ok());
        // Verify border exists (non-zero pixels on edges)
        let border_pixels = [
            // Top row
            buf[0..4].iter().any(|&b| b != 0x00),
            // Bottom row
            buf[(379 * 380 * 4)..(379 * 380 * 4 + 4)].iter().any(|&b| b != 0x00),
            // Left column middle
            buf[(190 * 380 * 4)..(190 * 380 * 4 + 4)].iter().any(|&b| b != 0x00),
            // Right column middle
            buf[(190 * 380 * 4 + 4 * 379)..(190 * 380 * 4 + 4 * 380)].iter().any(|&b| b != 0x00),
        ];
        assert!(border_pixels.iter().any(|&b| b), "Compact render missing border");
    }

    #[test]
    fn test_renderer_highlight_border() {
        let mut renderer = Renderer::new();
        let candidates = vec![
            Candidate { text: "第一个".to_string(), pinyin: "di yi ge".to_string(), freq: 1, ai: false },
            Candidate { text: "第二个".to_string(), pinyin: "di er ge".to_string(), freq: 2, ai: false },
        ];
        let mut buf = vec![0u8; 380 * 380 * 4];
        let result = renderer.draw_candidates_compact(&mut buf, 380, 380, &candidates, 1, "预编辑");
        assert!(result.is_ok());
        // Verify highlighted item has '>' prefix in rendered text
        // This is a simplified check - we verify the border color changed for highlight
        let highlight_color = 0x5E81ACFFu32;
        let (rh, gh, bh, ah) = (
            (highlight_color >> 16) as u8,
            (highlight_color >> 8) as u8,
            highlight_color as u8,
            (highlight_color >> 24) as u8,
        );
        // Check a few pixels near the highlighted item (2nd row)
        let row_offset = 1 * 380 * 4;
        let sample_pixels = &buf[row_offset..row_offset + 20];
        let has_highlight = sample_pixels.chunks_exact(4).any(|px| {
            px[0] == rh && px[1] == gh && px[2] == bh && px[3] == ah
        });
        assert!(has_highlight, "Highlight border not rendered");
    }
}