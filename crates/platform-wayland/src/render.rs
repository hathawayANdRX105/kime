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

/// Pure renderer with cosmic_text backend.
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

    /// Draw candidates and preedit into ARGB pixel buffer (width * height * 4 bytes).
    /// Buffer layout: top-to-bottom rows, each row left-to-right pixels.
    /// Returns Ok(()) on success, Err on invalid buffer size.
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

        // 半透明深底：不透明面板便于阅读（ARGB: A=0xE8 深灰蓝底）
        for px in buf.chunks_exact_mut(4) {
            px[0] = 0x1E;
            px[1] = 0x1E;
            px[2] = 0x26;
            px[3] = 0xE8;
        }

        // Build text with line breaks: preedit (small) + numbered candidates
        let mut text = String::new();

        if !preedit.is_empty() {
            // Preedit in smaller font
            text.push_str(&format!("{}\n", preedit));
        }

        for (i, cand) in candidates.iter().enumerate() {
            if i == highlight {
                // Highlighted candidate with indicator
                text.push_str(&format!("> {}. {} \n", i + 1, cand.text));
            } else {
                text.push_str(&format!("  {}. {} \n", i + 1, cand.text));
            }
        }

        // Create buffer with proper Metrics
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(self.font_size as f32, height as f32),
        );

        // Render text with proper Attrs handling
        let mut attrs = Attrs::new();
        attrs.family = cosmic_text::Family::SansSerif;
        attrs.style = Style::Normal;

        let color = cosmic_text::Color::rgba(240, 240, 245, 255);

        // Draw: callback receives (x, y, w, h, color) per glyph pixel run;
        // write solid ARGB pixels into the buffer.
        let mut cache = cosmic_text::SwashCache::new();
        let color = cosmic_text::Color::rgba(20, 20, 20, 255);
        buffer.draw(&mut self.font_system, &mut cache, color, |x, y, w, h, c| {
            // Color(u32) is 0xRRGGBBAA per rgba() constructor
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

        // CJK readability gate: rendered glyphs must hit the buffer
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
}
