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

/// 读 off 处一页字节，返回从 off 起一页消费多少字节。
fn consumed_at_bitmap(
    book_f: &mut ActualFile, buf: &mut [u8], off: u32,
    max_w: i32, full_w: i32, half_w: i32, max_lines: u32,
) -> u32 {
    let _ = book_f.seek_from_start(off);
    let mut got = 0usize;
    while got < buf.len() {
        match book_f.read(&mut buf[got..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    compute_consumed_bitmap(buf, got, max_w, full_w, half_w, max_lines) as u32
}

/// 把 off 对齐到 ≤ off 的最近 UTF-8 字符起始（读最多 4 字节回退到非续字节）。
/// 半字符起点会把续字节当满宽 FFFD 扭曲页大小，测试前必须对齐到字符起点。
fn align_down_char(book_f: &mut ActualFile, off: u32) -> u32 {
    if off == 0 {
        return 0;
    }
    let back = if off > 3 { 3 } else { off };
    let start = off - back;
    let want = (back + 1) as usize;
    let mut tmp = [0u8; 4];
    let _ = book_f.seek_from_start(start);
    let mut got = 0usize;
    while got < want {
        match book_f.read(&mut tmp[got..want]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    let mut pos = off;
    loop {
        let idx = (pos - start) as usize;
        if idx >= got {
            return start;
        }
        if (tmp[idx] & 0xC0) != 0x80 {
            return pos; // 非续字节 = 字符起始
        }
        if pos == 0 {
            return 0;
        }
        pos -= 1;
    }
}

/// 返回 cur_start 上一页的准确起始偏移（历史栈空、需后退时用）。二分+字符对齐+≤，快且稳。
///
/// 真页边界 pk-1 满足 pk-1 + 一页消费 ≤ cur_start 且最接近（该页在 cur_start 处或之前结束）。
/// 二分（仅在字符对齐点测）缩小范围，再从 hi 向下找首个字符对齐的 P-true = pk-1。
/// **用 ≤ 非 ==**：== 在 consumed 偶有 ±1 字节误差（缓冲边界等）时命中不到 → 返回 0 → 跳第一页；
/// ≤ 必有解（lo 兜底），永不跳页。字符对齐保证首字不截断。只扫 ~几十页，正常后退走历史栈不经过这里。
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
    // 二分（字符对齐 mid）：最大 X 使 X+consumed(X) ≤ cur_start。lo 始终保持 P-true。
    let mut lo: u32 = 0;
    let mut hi: u32 = cur_start;
    while lo + 16 < hi {
        let mid = align_down_char(book_f, (lo + hi) / 2);
        if mid <= lo {
            break;
        }
        let c = consumed_at_bitmap(book_f, buf, mid, max_w, full_w, half_w, max_lines);
        if c > 0 && mid + c <= cur_start {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // 精化（字符对齐、≤）：从 hi 向下找首个 P-true = 最大字符对齐 P-true = pk-1
    let mut x = align_down_char(book_f, hi);
    let mut steps = 0u32;
    while steps < 32 {
        let c = consumed_at_bitmap(book_f, buf, x, max_w, full_w, half_w, max_lines);
        if c > 0 && x + c <= cur_start {
            return x;
        }
        if x == 0 {
            break;
        }
        x = align_down_char(book_f, x - 1);
        steps += 1;
    }
    lo // 兜底：lo 是字符对齐 P-true（二分保证），绝不返回 0-失败
}
