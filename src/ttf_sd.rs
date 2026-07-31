//! SD 卡 TTF 按需随机读取字形（不整文件载入）。
//!
//! ab_glyph 要求整字体为连续 &[u8]，放不下 RAM；故自行实现 TTF 解析：
//! 表目录 → head/maxp/hhea → cmap → loca → hmtx → glyf，逐字形从 SD 随机读取，
//! 二次曲线展平 + 奇偶扫描线填充。算法已在 ttf_spike/render_ref.py 对照 PIL 验证。
#![cfg(feature = "ttf_spike")]
#![allow(dead_code)]

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Dimensions, Point};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_sdmmc::VolumeIdx;
use esp_println::println;

use crate::display::EpdDisplay;
use crate::sd_mount::{ActualFile, SdMount, SD_MOUNT};

const SAMPLE: &str = include_str!("../ttf_spike/sample.txt");

// ── 单字形渲染工作区（静态 .bss，避免占堆）。容量偏保守以省 DRAM 让给主栈；
// 极复杂字形会被裁剪（spike 可接受）。
const MAX_PTS: usize = 300;
const MAX_CONTOURS: usize = 64;
const MAX_SEGS: usize = 400;
const MAX_CROSS: usize = 150;
static mut PTS_X: [i32; MAX_PTS] = [0; MAX_PTS];
static mut PTS_Y: [i32; MAX_PTS] = [0; MAX_PTS];
static mut PTS_ON: [u8; MAX_PTS] = [0; MAX_PTS];
static mut END_PTS: [u16; MAX_CONTOURS] = [0; MAX_CONTOURS];
static mut SEG: [f32; MAX_SEGS * 4] = [0.0; MAX_SEGS * 4];
static mut CROSS_X: [f32; MAX_CROSS] = [0.0; MAX_CROSS];
// cmap 子表整块缓存（simhei 仅 1.3KB）：查表零 SD 访问，否则每字上千次 seek 会饿死系统
const CMAP_CACHE_MAX: usize = 2048;
static mut CMAP_CACHE: [u8; CMAP_CACHE_MAX] = [0u8; CMAP_CACHE_MAX];
static mut CMAP_LEN: u32 = 0;
// 单字形整块读缓冲：避免解析时每个 u8/u16 都 seek 一次
const GLYF_BUF_MAX: usize = 1536;
static mut GLYF_BUF: [u8; GLYF_BUF_MAX] = [0u8; GLYF_BUF_MAX];
// 读书一页的字节缓冲（即时分页用）：调用方 read_book_chunk 填充，paginate_render 消费
const BOOK_BUF_MAX: usize = 4096;
static mut BOOK_BUF: [u8; BOOK_BUF_MAX] = [0u8; BOOK_BUF_MAX];

// ── 字形位图缓存：渲染过的字形存 1-bit 位图，跨页复用，避免重复读 SD + 重复光栅化 ──
const GC_N: usize = 128;          // 缓存槽位数（~一页唯一字数）
const GC_BMP_BYTES: usize = 80;   // 单字形位图上限（~24×24/8≈72）
static mut GC_GID: [u16; GC_N] = [0xFFFF; GC_N]; // 0xFFFF = 空
static mut GC_W: [u8; GC_N] = [0; GC_N];
static mut GC_H: [u8; GC_N] = [0; GC_N];
static mut GC_BMP: [[u8; GC_BMP_BYTES]; GC_N] = [[0u8; GC_BMP_BYTES]; GC_N];
static mut GC_NEXT: usize = 0;    // 轮询分配位置

unsafe fn cu16(rel: u32) -> u16 {
    let base = core::ptr::addr_of_mut!(CMAP_CACHE).cast::<u8>();
    let mut b = [0u8; 2];
    b[0] = base.add(rel as usize).read();
    b[1] = base.add(rel as usize + 1).read();
    u16::from_be_bytes(b)
}
unsafe fn cu32(rel: u32) -> u32 {
    let base = core::ptr::addr_of_mut!(CMAP_CACHE).cast::<u8>();
    let mut b = [0u8; 4];
    for i in 0..4 {
        b[i] = base.add(rel as usize + i).read();
    }
    u32::from_be_bytes(b)
}

pub struct FontInfo {
    units_per_em: u16,
    loc_fmt: i16,
    num_glyphs: u32,
    num_h_metrics: u16,
    loca_off: u32,
    glyf_off: u32,
    hmtx_off: u32,
    cmap_sub: u32,
    cmap_fmt: u16,
}

// ── 随机读取 ──
fn read_at(f: &mut ActualFile, off: u32, dst: &mut [u8]) {
    let _ = f.seek_from_start(off);
    let mut got = 0;
    while got < dst.len() {
        match f.read(&mut dst[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
}
/// 读进裸指针（静态缓冲），绕开 edition 2024 static_mut_refs 限制。
unsafe fn read_into(f: &mut ActualFile, off: u32, dst: *mut u8, len: usize) {
    read_at(f, off, core::slice::from_raw_parts_mut(dst, len));
}
fn u8_at(f: &mut ActualFile, o: u32) -> u8 {
    let mut b = [0u8; 1];
    read_at(f, o, &mut b);
    b[0]
}
fn u16_at(f: &mut ActualFile, o: u32) -> u16 {
    let mut b = [0u8; 2];
    read_at(f, o, &mut b);
    u16::from_be_bytes(b)
}
fn i16_at(f: &mut ActualFile, o: u32) -> i16 {
    u16_at(f, o) as i16
}
fn u32_at(f: &mut ActualFile, o: u32) -> u32 {
    let mut b = [0u8; 4];
    read_at(f, o, &mut b);
    u32::from_be_bytes(b)
}

unsafe fn pts_on(i: usize) -> u8 {
    core::ptr::addr_of_mut!(PTS_ON).cast::<u8>().add(i).read()
}
unsafe fn set_xy(i: usize, x: i32, y: i32) {
    core::ptr::addr_of_mut!(PTS_X).cast::<i32>().add(i).write(x);
    core::ptr::addr_of_mut!(PTS_Y).cast::<i32>().add(i).write(y);
}
unsafe fn seg_push(seg_n: &mut usize, x0: f32, y0: f32, x1: f32, y1: f32) {
    if *seg_n < MAX_SEGS {
        let b = core::ptr::addr_of_mut!(SEG).cast::<f32>().add(*seg_n * 4);
        b.write(x0);
        b.add(1).write(y0);
        b.add(2).write(x1);
        b.add(3).write(y1);
        *seg_n += 1;
    }
}
unsafe fn seg_quad(seg_n: &mut usize, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
    let n2 = 6;
    let (mut cx, mut cy) = p0;
    for k in 1..=n2 {
        let t = k as f32 / n2 as f32;
        let mt = 1.0 - t;
        let ex = mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0;
        let ey = mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1;
        seg_push(seg_n, cx, cy, ex, ey);
        cx = ex;
        cy = ey;
    }
}

fn parse_font_info(f: &mut ActualFile) -> Option<FontInfo> {
    let num_tables = u16_at(f, 4);
    let mut off = 12u32;
    let mut loc = 0u32;
    let mut glyf = 0u32;
    let mut hmtx = 0u32;
    let mut cmap = 0u32;
    let mut head = 0u32;
    let mut maxp = 0u32;
    let mut hhea = 0u32;
    let mut buf = [0u8; 4];
    for _ in 0..num_tables {
        read_at(f, off, &mut buf);
        let toff = u32_at(f, off + 8);
        match &buf {
            b"loca" => loc = toff,
            b"glyf" => glyf = toff,
            b"hmtx" => hmtx = toff,
            b"cmap" => cmap = toff,
            b"head" => head = toff,
            b"maxp" => maxp = toff,
            b"hhea" => hhea = toff,
            _ => {}
        }
        off += 16;
    }
    if head == 0 || glyf == 0 {
        return None;
    }
    let units_per_em = u16_at(f, head + 18);
    let loc_fmt = i16_at(f, head + 50);
    let num_glyphs = u16_at(f, maxp + 4) as u32;
    let num_h_metrics = u16_at(f, hhea + 34);
    let nrec = u16_at(f, cmap + 2);
    let mut best: Option<(u8, u32, u16)> = None;
    for i in 0..nrec {
        let rec = cmap + 4 + i as u32 * 8;
        let pid = u16_at(f, rec);
        let eid = u16_at(f, rec + 2);
        let so = u32_at(f, rec + 4);
        let fmt = u16_at(f, cmap + so);
        if pid == 0 || (pid == 3 && (eid == 1 || eid == 10)) {
            let score = if fmt == 12 { 2 } else { 1 };
            if best.is_none() || score > best.unwrap().0 {
                best = Some((score, cmap + so, fmt));
            }
        }
    }
    let (_, cmap_sub, cmap_fmt) = best?;
    // 整块读 cmap 子表进 RAM 缓存（查表不再访问 SD）
    // 注意：format 4 的 length 是 u16（@+2），format 12 的 length 才是 u32（@+2）。
    let sub_len = if cmap_fmt == 12 {
        u32_at(f, cmap_sub + 2) as usize
    } else {
        u16_at(f, cmap_sub + 2) as usize
    };
    if sub_len > 0 && sub_len <= CMAP_CACHE_MAX {
        unsafe {
            let dst = core::ptr::addr_of_mut!(CMAP_CACHE).cast::<u8>();
            let mut tmp = [0u8; 512];
            let mut off = 0u32;
            while off < sub_len as u32 {
                let n = (sub_len as u32 - off).min(512) as usize;
                read_at(f, cmap_sub + off, &mut tmp[..n]);
                for i in 0..n {
                    dst.add(off as usize + i).write(tmp[i]);
                }
                off += n as u32;
            }
            core::ptr::addr_of_mut!(CMAP_LEN).write(sub_len as u32);
        }
    }
    Some(FontInfo {
        units_per_em,
        loc_fmt,
        num_glyphs,
        num_h_metrics,
        loca_off: loc,
        glyf_off: glyf,
        hmtx_off: hmtx,
        cmap_sub,
        cmap_fmt,
    })
}

fn char_to_gid(_f: &mut ActualFile, fi: &FontInfo, ch: char) -> u16 {
    let c = ch as u32;
    // 从 RAM 缓存读取，零 SD 访问
    if fi.cmap_fmt == 12 {
        let ng = unsafe { cu32(12) };
        let base = 16u32;
        let mut lo = 0i64;
        let mut hi = ng as i64 - 1;
        while lo <= hi {
            let m = ((lo + hi) / 2) as u32;
            let g = base + m * 12;
            let st = unsafe { cu32(g) };
            let en = unsafe { cu32(g + 4) };
            let sg = unsafe { cu32(g + 8) };
            if c < st {
                hi = m as i64 - 1;
            } else if c > en {
                lo = m as i64 + 1;
            } else {
                return (sg + (c - st)) as u16;
            }
        }
    } else if fi.cmap_fmt == 4 {
        let seg = unsafe { cu16(6) } as u32 / 2;
        let eb = 14u32;
        let sb = eb + seg * 2 + 2;
        let db = sb + seg * 2;
        let rb = db + seg * 2;
        let mut i = 0u32;
        while i < seg {
            if unsafe { cu16(eb + i * 2) } as u32 >= c {
                let start = unsafe { cu16(sb + i * 2) } as u32;
                if start > c {
                    return 0;
                }
                let d = unsafe { cu16(db + i * 2) } as i16 as i32;
                let ro = unsafe { cu16(rb + i * 2) } as u32;
                if ro == 0 {
                    return ((c as i32 + d) & 0xFFFF) as u16;
                }
                let gid_addr = rb + i * 2 + ro + 2 * (c - start);
                let gid = unsafe { cu16(gid_addr) };
                return if gid == 0 {
                    0
                } else {
                    ((gid as i32 + d) & 0xFFFF) as u16
                };
            }
            i += 1;
        }
    }
    0
}

fn advance(f: &mut ActualFile, fi: &FontInfo, gid: u16) -> u16 {
    if (gid as u32) < fi.num_h_metrics as u32 {
        u16_at(f, fi.hmtx_off + gid as u32 * 4)
    } else if fi.num_h_metrics > 0 {
        u16_at(f, fi.hmtx_off + (fi.num_h_metrics as u32 - 1) * 4)
    } else {
        0
    }
}

fn parse_glyph(f: &mut ActualFile, fi: &FontInfo, gid: u16) -> Option<(usize, usize, [i16; 4])> {
    let (a, b) = if fi.loc_fmt == 1 {
        let a = u32_at(f, fi.loca_off + gid as u32 * 4);
        let b = u32_at(f, fi.loca_off + (gid as u32 + 1) * 4);
        (a, b)
    } else {
        let a = u16_at(f, fi.loca_off + gid as u32 * 2) as u32 * 2;
        let b = u16_at(f, fi.loca_off + (gid as u32 + 1) * 2) as u32 * 2;
        (a, b)
    };
    let glen = b.saturating_sub(a) as usize;
    if glen == 0 || glen > GLYF_BUF_MAX {
        return None;
    }
    // 整块读字形进 RAM，之后解析零 SD 访问
    unsafe {
        read_into(
            f,
            fi.glyf_off + a,
            core::ptr::addr_of_mut!(GLYF_BUF) as *mut u8,
            glen,
        );
    }
    let g = |rel: usize| -> u8 { unsafe { core::ptr::addr_of_mut!(GLYF_BUF).cast::<u8>().add(rel).read() } };
    let gu16 = |rel: usize| -> u16 { unsafe { u16::from_be_bytes([g(rel), g(rel + 1)]) } };
    let gi16 = |rel: usize| -> i16 { gu16(rel) as i16 };

    let nc = gi16(0);
    if nc < 0 || nc as usize > MAX_CONTOURS {
        return None;
    }
    let nc = nc as usize;
    let bbox = [gi16(2), gi16(4), gi16(6), gi16(8)];
    let mut p = 10usize;
    for i in 0..nc {
        unsafe {
            core::ptr::addr_of_mut!(END_PTS).cast::<u16>().add(i).write(gu16(p + i * 2));
        }
    }
    p += nc * 2;
    let npt = if nc > 0 {
        unsafe { core::ptr::addr_of_mut!(END_PTS).cast::<u16>().add(nc - 1).read() as usize + 1 }
    } else {
        return None;
    };
    if npt > MAX_PTS {
        return None;
    }
    let instr_len = gu16(p);
    p += 2 + instr_len as usize;
    // flags（带 repeat）
    let mut got = 0usize;
    while got < npt {
        let fl = g(p);
        p += 1;
        unsafe {
            core::ptr::addr_of_mut!(PTS_ON).cast::<u8>().add(got).write(fl);
        }
        got += 1;
        if fl & 0x08 != 0 {
            let rep = g(p) as usize;
            p += 1;
            for _ in 0..rep {
                if got >= npt {
                    break;
                }
                unsafe {
                    core::ptr::addr_of_mut!(PTS_ON).cast::<u8>().add(got).write(fl);
                }
                got += 1;
            }
        }
    }
    // x
    let mut x: i32 = 0;
    for i in 0..npt {
        let fl = unsafe { pts_on(i) };
        if fl & 0x02 != 0 {
            let dx = g(p) as i32;
            p += 1;
            x += if fl & 0x10 != 0 { dx } else { -dx };
        } else if fl & 0x10 == 0 {
            x += gi16(p) as i32;
            p += 2;
        }
        unsafe {
            set_xy_x(i, x);
        }
    }
    // y
    let mut y: i32 = 0;
    for i in 0..npt {
        let fl = unsafe { pts_on(i) };
        if fl & 0x04 != 0 {
            let dy = g(p) as i32;
            p += 1;
            y += if fl & 0x20 != 0 { dy } else { -dy };
        } else if fl & 0x20 == 0 {
            y += gi16(p) as i32;
            p += 2;
        }
        unsafe { set_xy_y(i, y); }
    }
    Some((npt, nc, bbox))
}

unsafe fn set_xy_x(i: usize, x: i32) {
    core::ptr::addr_of_mut!(PTS_X).cast::<i32>().add(i).write(x);
}
unsafe fn set_xy_y(i: usize, y: i32) {
    core::ptr::addr_of_mut!(PTS_Y).cast::<i32>().add(i).write(y);
}

unsafe fn flatten_contour(
    mut seg_n: usize, s: usize, e: usize, sc: f32, x0: i32, y1: i32,
) -> usize {
    let n = e - s + 1;
    if n == 0 {
        return seg_n;
    }
    let px = core::ptr::addr_of_mut!(PTS_X).cast::<i32>();
    let py = core::ptr::addr_of_mut!(PTS_Y).cast::<i32>();
    let pon = core::ptr::addr_of_mut!(PTS_ON).cast::<u8>();
    let tx = |xi: i32| (xi - x0) as f32 * sc;
    let ty = |yi: i32| (y1 - yi) as f32 * sc;

    let mut start = 0usize;
    while start < n {
        if pon.add(s + start).read() & 1 != 0 {
            break;
        }
        start += 1;
    }
    if start == n {
        // 全 off：合成中点起点，相邻中点为隐式 on
        let mut vcur = (
            (tx(px.add(s + n - 1).read()) + tx(px.add(s).read())) / 2.0,
            (ty(py.add(s + n - 1).read()) + ty(py.add(s).read())) / 2.0,
        );
        for k in 0..n {
            let ctrl = (tx(px.add(s + k).read()), ty(py.add(s + k).read()));
            let nxt = (tx(px.add(s + (k + 1) % n).read()), ty(py.add(s + (k + 1) % n).read()));
            let end = ((ctrl.0 + nxt.0) / 2.0, (ctrl.1 + nxt.1) / 2.0);
            seg_quad(&mut seg_n, vcur, ctrl, end);
            vcur = end;
        }
        return seg_n;
    }
    let idx = |k: usize| s + (start + k) % n;
    let v0 = (tx(px.add(idx(0)).read()), ty(py.add(idx(0)).read()));
    let mut vcur = v0;
    let mut k = 1usize;
    while k < n {
        let ison = pon.add(idx(k)).read() & 1 != 0;
        if ison {
            let p = (tx(px.add(idx(k)).read()), ty(py.add(idx(k)).read()));
            seg_push(&mut seg_n, vcur.0, vcur.1, p.0, p.1);
            vcur = p;
            k += 1;
        } else {
            let ctrl = (tx(px.add(idx(k)).read()), ty(py.add(idx(k)).read()));
            if k + 1 < n && pon.add(idx(k + 1)).read() & 1 != 0 {
                let end = (tx(px.add(idx(k + 1)).read()), ty(py.add(idx(k + 1)).read()));
                seg_quad(&mut seg_n, vcur, ctrl, end);
                vcur = end;
                k += 2;
            } else {
                let nxt = (tx(px.add(idx(k + 1)).read()), ty(py.add(idx(k + 1)).read()));
                let end = ((ctrl.0 + nxt.0) / 2.0, (ctrl.1 + nxt.1) / 2.0);
                seg_quad(&mut seg_n, vcur, ctrl, end);
                vcur = end;
                k += 1;
            }
        }
    }
    if vcur != v0 {
        seg_push(&mut seg_n, vcur.0, vcur.1, v0.0, v0.1);
    }
    seg_n
}

unsafe fn fill_glyph<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D, seg_n: usize, ox: i32, oy: i32, w: i32, h: i32, color: BinaryColor,
) {
    let seg = core::ptr::addr_of_mut!(SEG).cast::<f32>();
    let cross = core::ptr::addr_of_mut!(CROSS_X).cast::<f32>();
    for y in 0..h {
        let yc = y as f32 + 0.5;
        let mut nc = 0usize;
        for s in 0..seg_n {
            let b = seg.add(s * 4);
            let (ax, ay) = (b.read(), b.add(1).read());
            let (bx, by) = (b.add(2).read(), b.add(3).read());
            if (ay <= yc && yc < by) || (by <= yc && yc < ay) {
                if nc < MAX_CROSS {
                    let t = if by != ay { (yc - ay) / (by - ay) } else { 0.0 };
                    cross.add(nc).write(ax + t * (bx - ax));
                    nc += 1;
                }
            }
        }
        // 插入排序
        for i in 1..nc {
            let mut j = i;
            while j > 0 {
                let a = cross.add(j - 1).read();
                let b = cross.add(j).read();
                if a > b {
                    cross.add(j - 1).write(b);
                    cross.add(j).write(a);
                    j -= 1;
                } else {
                    break;
                }
            }
        }
        let mut k = 0;
        while k + 1 < nc {
            let xs = cross.add(k).read();
            let xe = cross.add(k + 1).read();
            let x0i = (if xs < 0.0 { 0.0 } else { xs }) as i32;
            let x1i = (if xe > w as f32 { w as f32 } else { xe }) as i32;
            for x in x0i..x1i {
                let _ = display.draw_iter(core::iter::once(Pixel(
                    Point::new(ox + x, oy + y), color,
                )));
            }
            k += 2;
        }
    }
}

/// 解析已打开的字体文件（表目录 + 缓存 cmap）。不锁 SD，文件由调用方打开。
pub fn open_font(file: &mut ActualFile) -> Option<FontInfo> {
    parse_font_info(file)
}

/// 用已打开的字体把一段文本渲染进 display（逐字按需随机读字形）。
/// `text` 中的 `\n` 断行；超过 `max_width` 兜底换行；到屏幕底部停止。
/// 不锁 SD——文件由调用方打开（避免与已持有 SD_MOUNT 的调用方死锁）。
pub fn render_string(
    file: &mut ActualFile,
    fi: &FontInfo,
    display: &mut EpdDisplay,
    text: &str,
    px: f32,
    top_left: Point,
    max_width: i32,
    line_h: i32,
) {
    let vh = display.bounding_box().size.height as i32;
    let sc = px / fi.units_per_em as f32;
    let left = top_left.x;
    let mut pen_x = left;
    let mut pen_y = top_left.y;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            pen_x = left;
            pen_y += line_h;
            continue;
        }
        let gid = char_to_gid(file, fi, ch);
        let adv_f = advance(file, fi, gid) as f32 * sc;
        if pen_x + adv_f as i32 > left + max_width {
            pen_x = left;
            pen_y += line_h;
        }
        if pen_y + line_h > vh {
            break;
        }
        if let Some((npt, ncon, bbox)) = parse_glyph(file, fi, gid) {
            let x0 = bbox[0] as i32;
            let x1 = bbox[2] as i32;
            let y1 = bbox[3] as i32;
            let gw = ((x1 - x0) as f32 * sc) as i32 + 1;
            let gh = ((bbox[3] - bbox[1]) as f32 * sc) as i32 + 1;
            let mut seg_n = 0usize;
            let mut si = 0usize;
            for ci in 0..ncon {
                let ei = unsafe {
                    core::ptr::addr_of_mut!(END_PTS).cast::<u16>().add(ci).read() as usize
                };
                if ei >= npt {
                    break;
                }
                seg_n = unsafe { flatten_contour(seg_n, si, ei, sc, x0, y1) };
                si = ei + 1;
            }
            unsafe { fill_glyph(display, seg_n, pen_x, pen_y, gw, gh, BinaryColor::On) };
        }
        pen_x += adv_f as i32;
    }
}

/// 缓存查表：命中返回槽位号。
unsafe fn cache_lookup(gid: u16) -> Option<usize> {
    let g = core::ptr::addr_of!(GC_GID).cast::<u16>();
    let mut i = 0;
    while i < GC_N {
        if g.add(i).read() == gid {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 分配槽位（轮询覆盖），记录 gid/w/h 并清空位图区，返回槽位号。
unsafe fn cache_store(gid: u16, w: u8, h: u8) -> usize {
    let next = core::ptr::addr_of_mut!(GC_NEXT);
    let slot = next.read();
    *next = (slot + 1) % GC_N;
    core::ptr::addr_of_mut!(GC_GID).cast::<u16>().add(slot).write(gid);
    core::ptr::addr_of_mut!(GC_W).cast::<u8>().add(slot).write(w);
    core::ptr::addr_of_mut!(GC_H).cast::<u8>().add(slot).write(h);
    let bmp = core::ptr::addr_of_mut!(GC_BMP).cast::<u8>().add(slot * GC_BMP_BYTES);
    let mut i = 0;
    while i < GC_BMP_BYTES {
        bmp.add(i).write(0);
        i += 1;
    }
    slot
}

/// 把已展平的 SEG 奇偶填充进 GC_BMP[slot]（1-bit，gw×gh）。
unsafe fn fill_into_bitmap(seg_n: usize, gw: i32, gh: i32, slot: usize) {
    let rowbytes = ((gw as usize) + 7) / 8;
    let bmp = core::ptr::addr_of_mut!(GC_BMP).cast::<u8>().add(slot * GC_BMP_BYTES);
    let seg = core::ptr::addr_of!(SEG).cast::<f32>();
    let cross = core::ptr::addr_of_mut!(CROSS_X).cast::<f32>();
    let mut y = 0;
    while y < gh {
        let yc = y as f32 + 0.5;
        let mut nc = 0usize;
        let mut s = 0;
        while s < seg_n {
            let b = seg.add(s * 4);
            let (ax, ay) = (b.read(), b.add(1).read());
            let (bx, by) = (b.add(2).read(), b.add(3).read());
            if (ay <= yc && yc < by) || (by <= yc && yc < ay) {
                if nc < MAX_CROSS {
                    let t = if by != ay { (yc - ay) / (by - ay) } else { 0.0 };
                    cross.add(nc).write(ax + t * (bx - ax));
                    nc += 1;
                }
            }
            s += 1;
        }
        let mut i = 1;
        while i < nc {
            let mut j = i;
            while j > 0 {
                let a = cross.add(j - 1).read();
                let bv = cross.add(j).read();
                if a > bv {
                    cross.add(j - 1).write(bv);
                    cross.add(j).write(a);
                    j -= 1;
                } else {
                    break;
                }
            }
            i += 1;
        }
        let mut k = 0;
        while k + 1 < nc {
            let xs = cross.add(k).read();
            let xe = cross.add(k + 1).read();
            let mut x = (if xs < 0.0 { 0.0 } else { xs }) as i32;
            let xend = (if xe > gw as f32 { gw as f32 } else { xe }) as i32;
            while x < xend {
                if x >= 0 && (x as usize) < gw as usize {
                    let idx = (y as usize) * rowbytes + (x as usize) / 8;
                    let mask = 0x80u8 >> (x % 8);
                    bmp.add(idx).write(bmp.add(idx).read() | mask);
                }
                x += 1;
            }
            k += 2;
        }
        y += 1;
    }
}

/// 把 GC_BMP[slot] 的位图 blit 到 display（原点 ox,oy）。
unsafe fn blit_slot<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D, slot: usize, ox: i32, oy: i32,
) {
    let w = core::ptr::addr_of!(GC_W).cast::<u8>().add(slot).read() as i32;
    let h = core::ptr::addr_of!(GC_H).cast::<u8>().add(slot).read() as i32;
    let rowbytes = ((w as usize) + 7) / 8;
    let bmp = core::ptr::addr_of!(GC_BMP).cast::<u8>().add(slot * GC_BMP_BYTES);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let idx = (y as usize) * rowbytes + (x as usize) / 8;
            if bmp.add(idx).read() & (0x80u8 >> (x % 8)) != 0 {
                let _ = display.draw_iter(core::iter::once(Pixel(
                    Point::new(ox + x, oy + y), BinaryColor::On,
                )));
            }
            x += 1;
        }
        y += 1;
    }
}

/// 把书的字节从 offset 起读进 BOOK_BUF，返回读到的字节数。
pub fn read_book_chunk(f: &mut ActualFile, offset: u32) -> usize {
    let dst = core::ptr::addr_of_mut!(BOOK_BUF) as *mut u8;
    let _ = f.seek_from_start(offset);
    let mut got = 0usize;
    while got < BOOK_BUF_MAX {
        let buf = unsafe { core::slice::from_raw_parts_mut(dst.add(got), BOOK_BUF_MAX - got) };
        match f.read(buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    got
}

/// 即时分页+渲染：扫描 BOOK_BUF[..book_len] 的 UTF-8 文本，按 TTF advance 宽度换行，
/// 填满 max_lines 行即止，返回消费的字节数（下一页从此字节开始）。
pub fn paginate_render(
    f: &mut ActualFile,
    fi: &FontInfo,
    display: &mut EpdDisplay,
    book_len: usize,
    px: f32,
    top_left: Point,
    max_w: i32,
    line_h: i32,
    max_lines: u32,
) -> usize {
    if max_lines == 0 {
        return 0;
    }
    let vh = display.bounding_box().size.height as i32;
    let sc = px / fi.units_per_em as f32;
    let left = top_left.x;
    let mut pen_x = left;
    let mut pen_y = top_left.y;
    let mut line = 0u32;
    let mut i = 0usize;
    let buf = core::ptr::addr_of!(BOOK_BUF) as *const u8;
    while i < book_len {
        let b0 = unsafe { buf.add(i).read() };
        // UTF-8 首字节定字符字节长度（续字节不应出现在首位，按 1 跳过）
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
        if i + clen > book_len {
            break; // 缓冲末尾不完整字符，留给下一页
        }
        let ch = unsafe {
            let s = core::slice::from_raw_parts(buf.add(i), clen);
            core::str::from_utf8(s)
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or('\u{FFFD}')
        };
        if ch == '\n' || ch == '\r' {
            line += 1;
            i += clen;
            if line >= max_lines {
                break;
            }
            pen_x = left;
            pen_y += line_h;
            continue;
        }
        let gid = char_to_gid(f, fi, ch);
        let adv_f = advance(f, fi, gid) as f32 * sc;
        if pen_x + adv_f as i32 > left + max_w {
            line += 1;
            if line >= max_lines {
                break; // 此字留到下一页（i 未推进）
            }
            pen_x = left;
            pen_y += line_h;
        }
        if pen_y + line_h > vh {
            break;
        }
        // 字形位图缓存：命中直接 blit；未命中则光栅化进缓存再 blit（跨页复用）
        if let Some(slot) = unsafe { cache_lookup(gid) } {
            unsafe { blit_slot(display, slot, pen_x, pen_y) };
        } else if let Some((npt, ncon, bbox)) = parse_glyph(f, fi, gid) {
            let x0 = bbox[0] as i32;
            let x1 = bbox[2] as i32;
            let y1 = bbox[3] as i32;
            let gw = ((x1 - x0) as f32 * sc) as i32 + 1;
            let gh = ((bbox[3] - bbox[1]) as f32 * sc) as i32 + 1;
            let mut seg_n = 0usize;
            let mut si = 0usize;
            for ci in 0..ncon {
                let ei = unsafe {
                    core::ptr::addr_of_mut!(END_PTS).cast::<u16>().add(ci).read() as usize
                };
                if ei >= npt {
                    break;
                }
                seg_n = unsafe { flatten_contour(seg_n, si, ei, sc, x0, y1) };
                si = ei + 1;
            }
            let rowbytes = ((gw as usize) + 7) / 8;
            if gw > 0 && gh > 0 && (gh as usize) * rowbytes <= GC_BMP_BYTES {
                let slot = unsafe { cache_store(gid, gw as u8, gh as u8) };
                unsafe { fill_into_bitmap(seg_n, gw, gh, slot) };
                unsafe { blit_slot(display, slot, pen_x, pen_y) };
            } else {
                unsafe { fill_glyph(display, seg_n, pen_x, pen_y, gw, gh, BinaryColor::On) };
            }
        }
        pen_x += adv_f as i32;
        i += clen;
    }
    i
}

/// 主页 demo：从 SD 根目录 font.ttf 整屏渲染 SAMPLE。
pub async fn render_text(display: &mut EpdDisplay) {
    let vw = display.bounding_box().size.width as i32;
    let mut guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *guard else {
        println!("[ttf_sd] no SD");
        return;
    };
    let Ok(v) = sd.volume_manager.open_volume(VolumeIdx(0)) else {
        println!("[ttf_sd] open vol fail");
        return;
    };
    let Ok(mut root) = v.open_root_dir() else {
        println!("[ttf_sd] root dir fail");
        return;
    };
    let Ok(mut f) = SdMount::open_file_by_name(&mut root, "font.ttf", embedded_sdmmc::Mode::ReadOnly)
    else {
        println!("[ttf_sd] font.ttf not found");
        return;
    };
    let Some(fi) = open_font(&mut f) else {
        println!("[ttf_sd] parse font info fail");
        return;
    };
    println!(
        "[ttf_sd] upem={} glyphs={} loc_fmt={} cmap_fmt={}",
        fi.units_per_em, fi.num_glyphs, fi.loc_fmt, fi.cmap_fmt
    );
    render_string(&mut f, &fi, display, SAMPLE, 18.0, Point::new(6, 4), vw - 12, 20);
}
