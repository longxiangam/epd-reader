//! TTF 矢量字体光栅化 spike。
//!
//! 目的：验证 ab_glyph 能否在 riscv32imc / no_std 上编译、加它要多少 flash、
//! 光栅化一页汉字耗时多少。仅供验证，正式功能不入主流程。
//!
//! 启用：`--features ttf_spike`。烧录后从串口读 `[ttf_spike]` 行的耗时数据。
#![cfg(feature = "ttf_spike")]
#![allow(dead_code)]

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont, point};
use embassy_time::Instant;
use esp_println::println;
use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Dimensions, Point};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_sdmmc::VolumeIdx;

use crate::display::EpdDisplay;
use crate::sd_mount::{SdMount, SD_MOUNT};

/// 子集化的小 CJK TTF（由 ttf_spike/subset.py 从 simhei.ttf 抽取，约 50KB）。
const FONT_BYTES: &[u8] = include_bytes!("../ttf_spike/subset.ttf");

/// 一段示例阅读文本（书摘），用于测光栅化耗时。
const SAMPLE: &str = "电子墨水屏阅读器矢量字体任意大小显示，嵌入式设备渲染中文需要考虑内存与性能的平衡。春天来了，万物复苏，山野间一片新绿，他慢慢走过那条熟悉的小路，想起许多年前在这里度过的时光。月光洒在窗台上，书页微微翻动，远处传来几声犬吠，村庄已经沉睡，只有他还醒着，思考明天的方向。";

/// 光栅化重复次数（放大耗时，便于测量）。
const REPEATS: u32 = 5;

pub fn run() {
    println!("[ttf_spike] start; font_bytes={} sample_chars={}", FONT_BYTES.len(), SAMPLE.chars().count());

    let font = match FontRef::try_from_slice(FONT_BYTES) {
        Ok(f) => f,
        Err(e) => {
            println!("[ttf_spike] font parse FAILED: {:?}", e);
            return;
        }
    };
    println!(
        "[ttf_spike] parsed OK; glyph_count={} units_per_em={:?}",
        font.glyph_count(),
        font.units_per_em()
    );

    // 逐字号光栅化测时。关键：闭包必须把覆盖率累计进一个事后会被读取的值，
    // 否则 LLVM 会判定光栅化无副作用而整体优化掉，测出假的极小耗时。
    for &px in &[12.0_f32, 16.0, 24.0, 32.0] {
        let scale = PxScale::from(px);
        let scaled = font.as_scaled(scale);

        let t0 = Instant::now().as_millis();
        let mut glyphs = 0u32;
        let mut pixels: u64 = 0;
        let mut coverage_sum: u64 = 0;

        for _ in 0..REPEATS {
            for ch in SAMPLE.chars() {
                let glyph: Glyph = scaled.scaled_glyph(ch);
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let bb = outlined.px_bounds();
                    pixels += (bb.width().max(0.0) as u64) * (bb.height().max(0.0) as u64);
                    outlined.draw(|_x, _y, c| {
                        coverage_sum = coverage_sum.wrapping_add((c * 65535.0) as u64);
                    });
                    glyphs += 1;
                }
            }
        }

        let dt = Instant::now().as_millis() - t0;
        // 阻止编译器把累计值当常量折叠掉
        let _ = core::hint::black_box(coverage_sum);

        let per_glyph_ms = if glyphs > 0 { dt as f32 / glyphs as f32 } else { 0.0 };
        println!(
            "[ttf_spike] px={:>2}: glyphs={}  total={}ms  per-glyph={:.3}ms  raster-pixels={}",
            px as u8, glyphs, dt, per_glyph_ms, pixels
        );
    }
    println!("[ttf_spike] done.");
}

/// 用矢量字体在显示缓冲上画一行文字（端到端验证 TTF 光栅化）。
/// `px` = 像素字号，`top_left` = 文本块左上角，`color` = 前景色。
/// 逐字光栅化 → 覆盖率 > 0.5 的像素写进 DrawTarget。
pub fn draw_ttf_text<D>(
    display: &mut D,
    px: f32,
    text: &str,
    top_left: Point,
    color: BinaryColor,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    let font = match FontRef::try_from_slice(FONT_BYTES) {
        Ok(f) => f,
        Err(e) => {
            println!("[ttf_spike] draw: parse err {:?}", e);
            return;
        }
    };
    let scaled = font.as_scaled(PxScale::from(px));
    // 笔位置 = 基线原点；top_left 作为文本块左上角，下移 ascent 到基线
    let mut pen = point(top_left.x as f32, top_left.y as f32 + scaled.ascent());

    for ch in text.chars() {
        let glyph = font.glyph_id(ch).with_scale_and_position(px, pen);
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bb = outlined.px_bounds();
            let (min_x, min_y) = (bb.min.x as i32, bb.min.y as i32);
            outlined.draw(|gx, gy, c| {
                if c > 0.5 {
                    let _ = display.draw_iter(core::iter::once(Pixel(
                        Point::new(min_x + gx as i32, min_y + gy as i32),
                        color,
                    )));
                }
            });
        }
        // 前进到下一个字
        pen.x += scaled.h_advance(scaled.glyph_id(ch));
    }
}

/// 整屏渲染示例文本：与 sample.txt 字符集一致，font.ttf 覆盖全部字符。
const FULLSCREEN_TEXT: &str = include_str!("../ttf_spike/sample.txt");

/// 字体加载缓冲。放 .bss（不占堆）——运行时堆已被各任务 pinned future 占满，
/// 字体读进堆会 OOM。子集字体 ~18KB，留余量到 20KB。
/// 注意：DRAM 紧张，整文件加载决定了字体必须小子集；读真实整本书需要按需 SD 取字形。
const FONT_BUF_CAP: usize = 20480;
static mut FONT_BUF: [u8; FONT_BUF_CAP] = [0u8; FONT_BUF_CAP];

/// 从 SD 卡读取 font.ttf 进静态 .bss 缓冲，返回读取字节数。
/// edition 2024 禁止对 static mut 取引用，故用裸指针写。
async fn load_font_to_static() -> usize {
    let mut total = 0usize;
    let mut guard = SD_MOUNT.lock().await;
    if let Some(ref mut sd) = *guard {
        if let Ok(v) = sd.volume_manager.open_volume(VolumeIdx(0)) {
            if let Ok(mut root) = v.open_root_dir() {
                if let Ok(mut f) = SdMount::open_file_by_name(
                    &mut root,
                    "font.ttf",
                    embedded_sdmmc::Mode::ReadOnly,
                ) {
                    let mut chunk = [0u8; 512];
                    while total < FONT_BUF_CAP {
                        match f.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                let take = core::cmp::min(n, FONT_BUF_CAP - total);
                                unsafe {
                                    let dst = core::ptr::addr_of_mut!(FONT_BUF) as *mut u8;
                                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.add(total), take);
                                }
                                total += take;
                                if take < n {
                                    break; // 缓冲满，字体被截断
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    f.close();
                }
            }
        }
    }
    total
}

/// 从 SD 卡读取 font.ttf，整屏排版渲染示例文本。
/// 验证「SD 字体文件 + 矢量光栅化 + 整屏文字」端到端链路。
pub async fn render_full_screen(display: &mut EpdDisplay) {
    let len = load_font_to_static().await;
    if len == 0 {
        if let Ok(ef) = FontRef::try_from_slice(FONT_BYTES) {
            draw_ttf_text(display, 16.0, "SD font.ttf 读取失败", Point::new(4, 4), BinaryColor::On);
        }
        println!("[ttf_spike] SD font load FAILED (0 bytes)");
        return;
    }

    // 用裸指针构造切片，绕开 edition 2024 的 static_mut_refs 限制
    let font = {
        let slice = unsafe {
            core::slice::from_raw_parts(core::ptr::addr_of!(FONT_BUF) as *const u8, len)
        };
        match FontRef::try_from_slice(slice) {
            Ok(f) => {
                println!("[ttf_spike] font.ttf loaded: {} bytes", len);
                f
            }
            Err(e) => {
                if let Ok(ef) = FontRef::try_from_slice(FONT_BYTES) {
                    draw_ttf_text(display, 16.0, "font.ttf 解析失败", Point::new(4, 4), BinaryColor::On);
                }
                println!("[ttf_spike] font parse FAILED: {:?}", e);
                return;
            }
        }
    };

    // 整屏排版渲染（按字宽自动换行）
    let w = display.bounding_box().size.width;
    draw_ttf_paragraph(display, &font, 18.0, FULLSCREEN_TEXT, Point::new(6, 4), w.saturating_sub(12), BinaryColor::On, 2);
    println!("[ttf_spike] full-screen rendered from SD font");
}

/// 用矢量字体按字符宽度自动换行，整屏排版绘制文本（CJK 逐字换行，支持 \n）。
pub fn draw_ttf_paragraph<F, D>(
    display: &mut D,
    font: &F,
    px: f32,
    text: &str,
    top_left: Point,
    max_width: u32,
    color: BinaryColor,
    line_gap: u32,
) where
    F: Font,
    D: DrawTarget<Color = BinaryColor>,
{
    let scaled = font.as_scaled(PxScale::from(px));
    let line_h = px + line_gap as f32;
    let left = top_left.x as f32;
    let right = left + max_width as f32;
    let max_h = display.bounding_box().size.height as f32;
    let mut pen_x = left;
    let mut pen_y = top_left.y as f32 + scaled.ascent();

    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            pen_x = left;
            pen_y += line_h;
            continue;
        }
        let adv = scaled.h_advance(scaled.glyph_id(ch));
        if pen_x + adv > right {
            pen_x = left;
            pen_y += line_h;
        }
        if pen_y > max_h {
            break; // 超出屏幕底部
        }
        let glyph = font.glyph_id(ch).with_scale_and_position(px, point(pen_x, pen_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bb = outlined.px_bounds();
            let (mx, my) = (bb.min.x as i32, bb.min.y as i32);
            outlined.draw(|gx, gy, c| {
                if c > 0.5 {
                    let _ = display.draw_iter(core::iter::once(Pixel(
                        Point::new(mx + gx as i32, my + gy as i32),
                        color,
                    )));
                }
            });
        }
        pen_x += adv;
    }
}
