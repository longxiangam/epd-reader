//! 点阵字体（u8g2 内置 wqy）的等宽分页与渲染。
//!
//! 与 TTF 不同：宽度按固定单元格——中文 full_w、英文/数字 half_w（中文惯例半宽），
//! 不查询字形实际宽度。这样分页是纯算术、渲染是直接 blit 闪存里的位图，
//! 无光栅化、无 SD 字形读取 → 比 TTF 快得多。
//!
//! 单元格宽度由调用方按所选点阵字号给出（如 wqy16 → full=16, half=8）。
//! 渲染逐字画到单元格起点，pen_x 按宽度推进，与分页完全一致。

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::Point;
use epd_waveshare::color::Black;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use crate::sd_mount::ActualFile;

/// 一个 UTF-8 字符的单元格宽度：ASCII(<0x80，且非换行)→half_w，其余→full_w。
#[inline]
fn char_width(ch: char, full_w: i32, half_w: i32) -> i32 {
    if (ch as u32) < 0x80 {
        half_w
    } else {
        full_w
    }
}

/// 解析 buf[i..] 处一个 UTF-8 字符的（字节数, char）。非法续字节当 1 字节。
fn decode_char(buf: &[u8], i: usize, n: usize) -> Option<(usize, char)> {
    let b0 = buf[i];
    let clen = if b0 < 0x80 {
        1
    } else if b0 < 0xC0 {
        1
    } else if b0 < 0xE0 {
        2
    } else if b0 < 0xF0 {
        3
    } else {
        4
    };
    if i + clen > n {
        return None;
    }
    let ch = core::str::from_utf8(&buf[i..i + clen])
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('\u{FFFD}');
    Some((clen, ch))
}

/// 即时分页+渲染：扫描 buf[..n]，按固定宽度换行、满 max_lines 止，逐字 blit。
/// 返回本页消费的字节数。
pub fn paginate_render_bitmap<D: DrawTarget<Color = BinaryColor>>(
    font: &FontRenderer,
    buf: &[u8],
    n: usize,
    display: &mut D,
    top_left: Point,
    max_w: i32,
    line_h: i32,
    max_lines: u32,
    full_w: i32,
    half_w: i32,
) -> usize {
    if max_lines == 0 || n == 0 {
        return 0;
    }
    let left = top_left.x;
    let mut pen_x = left;
    let mut pen_y = top_left.y;
    let mut line = 0u32;
    let mut i = 0usize;
    while i < n {
        let (clen, ch) = match decode_char(buf, i, n) {
            Some(v) => v,
            None => break,
        };
        if ch == '\n' || ch == '\r' {
            i += clen;
            line += 1;
            if line >= max_lines {
                break;
            }
            pen_x = left;
            pen_y += line_h;
            continue;
        }
        let w = char_width(ch, full_w, half_w);
        if pen_x + w > left + max_w && pen_x > left {
            // 当前行放不下：换行，当前字不消费，下一轮重画
            line += 1;
            if line >= max_lines {
                break;
            }
            pen_x = left;
            pen_y += line_h;
            continue;
        }
        // 画到单元格起点（Top/Left 对齐）
        if let Ok(s) = core::str::from_utf8(&buf[i..i + clen]) {
            let _ = font.render_aligned(
                s,
                Point::new(pen_x, pen_y),
                VerticalPosition::Top,
                HorizontalAlignment::Left,
                FontColor::Transparent(Black),
                display,
            );
        }
        pen_x += w;
        i += clen;
    }
    i
}

/// 干跑分页（不渲染）：计算 buf[..n] 一页消费多少字节。find_prev_start_bitmap 用。
pub fn compute_consumed_bitmap(
    buf: &[u8],
    n: usize,
    max_w: i32,
    full_w: i32,
    half_w: i32,
    max_lines: u32,
) -> usize {
    if max_lines == 0 || n == 0 {
        return 0;
    }
    let left = 0i32;
    let mut pen_x = left;
    let mut line = 0u32;
    let mut i = 0usize;
    while i < n {
        let (clen, ch) = match decode_char(buf, i, n) {
            Some(v) => v,
            None => break,
        };
        if ch == '\n' || ch == '\r' {
            i += clen;
            line += 1;
            if line >= max_lines {
                break;
            }
            pen_x = left;
            continue;
        }
        let w = char_width(ch, full_w, half_w);
        if pen_x + w > left + max_w && pen_x > left {
            line += 1;
            if line >= max_lines {
                break;
            }
            pen_x = left;
            continue;
        }
        pen_x += w;
        i += clen;
    }
    i
}

/// 局部逆推：返回 cur_start 上一页的起始偏移（历史栈空、需后退时用）。
/// 从 cur_start 往前回退一段，读 chunk 进 buf，逐页 compute_consumed_bitmap 扫到 cur_start，
/// 取最后那个 < cur_start 的页起始。只扫几页，绝不扫全书。
pub fn find_prev_start_bitmap(
    book_f: &mut ActualFile,
    buf: &mut [u8],
    cur_start: u32,
    _file_len: u32,
    max_w: i32,
    full_w: i32,
    half_w: i32,
    max_lines: u32,
) -> u32 {
    if cur_start == 0 || max_lines == 0 {
        return 0;
    }
    let mut step: u32 = 3072;
    loop {
        let est = cur_start.saturating_sub(step);
        let mut off = est;
        let mut last = est;
        let mut reached = false;
        while off < cur_start {
            let _ = book_f.seek_from_start(off);
            let mut got = 0usize;
            while got < buf.len() {
                match book_f.read(&mut buf[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => got += k,
                }
            }
            if got == 0 {
                break;
            }
            let c = compute_consumed_bitmap(buf, got, max_w, full_w, half_w, max_lines) as u32;
            if c == 0 {
                break;
            }
            last = off;
            off = off.saturating_add(c);
            if off >= cur_start {
                reached = true;
                break;
            }
        }
        if reached && last < cur_start {
            return last;
        }
        if est == 0 {
            return 0;
        }
        step = step.saturating_mul(2);
    }
}
