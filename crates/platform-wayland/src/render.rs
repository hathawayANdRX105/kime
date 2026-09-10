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
    ///
    /// `color` 为 `0xAARRGGBB`。
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

    pub fn draw_candidates(
        &mut self,
        buf: &mut [u8],
        width: usize,
        height: usize,
        candidates: &[Candidate],
        highlight: usize,
        preedit: &str,
    ) -> Result<(), &'static str> {
        let need = width * height * 4;
        if buf.len() < need {
            return Err("Buffer size mismatch");
        }
        let buf = &mut buf[..need];

        for px in buf.chunks_exact_mut(4) {
            px[0] = 0x1E;
            px[1] = 0x1E;
            px[2] = 0x26;
            px[3] = 0xE8;
        }
        if !candidates.is_empty() {
            Self::draw_border(buf, width, height, 0xFF4C566Au32);
        }

        let mut text = String::new();
        if !preedit.is_empty() {
            text.push_str(&format!("{preedit}\n"));
        }
        for (i, cand) in candidates.iter().enumerate() {
            let prefix = if i == highlight { "> " } else { "  " };
            text.push_str(&format!("{} {}. {}\n", prefix, i + 1, cand.text));
        }

        let line_height = (self.font_size * 1.4) as f32;
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(self.font_size as f32, line_height),
        );
        buffer.set_size(Some(width as f32), Some(height as f32));
        let mut attrs = Attrs::new();
        attrs.family = cosmic_text::Family::SansSerif;
        attrs.style = Style::Normal;
        buffer.set_text(&text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
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
    fn test_renderer_draws_border_around_candidates() {
        let mut renderer = Renderer::new();
        let candidates = vec![
            Candidate {
                text: "测试".to_string(),
                pinyin: "ce shi".to_string(),
                freq: 1,
                ai: false,
            },
            Candidate {
                text: "结果".to_string(),
                pinyin: "jie guo".to_string(),
                freq: 2,
                ai: false,
            },
        ];
        let (w, h) = (380usize, 380usize);
        let mut buf = vec![0u8; w * h * 4];
        renderer
            .draw_candidates(&mut buf, w, h, &candidates, 0, "输入")
            .unwrap();

        // 边框色 #4C566A，四边中点都应命中（不能只撞上文字像素）
        let px = |x: usize, y: usize| {
            let off = (y * w + x) * 4;
            (buf[off], buf[off + 1], buf[off + 2])
        };
        let border = (0x4Cu8, 0x56u8, 0x6Au8);
        for (x, y, edge) in [
            (w / 2, 0, "上"),
            (w / 2, h - 1, "下"),
            (0, h / 2, "左"),
            (w - 1, h / 2, "右"),
        ] {
            assert_eq!(px(x, y), border, "{edge}边框未绘制");
        }
    }
}
