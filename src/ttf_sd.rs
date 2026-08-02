//! SD 卡 TTF 按需随机读取字形（不整文件载入）。
//!
//! 自实现 TTF 解析（表目录→cmap→loca→hmtx→glyf）逐字形从 SD seek+read，
//! 二次曲线展平 + 奇偶扫描线填充。所有工作区缓冲打包进 `TtfWs`，**堆分配**
//! （进阅读分配、退出释放），与 WiFi 的堆内存运行期互斥，不再常驻 .bss。
#![cfg(feature = "ttf_spike")]
#![allow(dead_code)]

use alloc::alloc::{alloc, dealloc, Layout};
use core::mem::size_of;
use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Dimensions, Point};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_sdmmc::VolumeIdx;
use esp_println::println;

use crate::display::EpdDisplay;
use crate::sd_mount::{ActualFile, SdMount};

// ── 缓冲容量（阅读模式堆宽裕，放宽以保证复杂 CJK 字形不截断）──
const MAX_PTS: usize = 400;
const MAX_CONTOURS: usize = 64;
const MAX_SEGS: usize = 512;
const MAX_CROSS: usize = 160;
const CMAP_CACHE_MAX: usize = 2048;
const HMTX_CACHE_MAX: usize = 1024;
const LOCA_CACHE_MAX: usize = 1024;
const GLYF_BUF_MAX: usize = 1024;
const BOOK_BUF_MAX: usize = 2048;
const GC_N: usize = 256;
const GC_BMP_BYTES: usize = 72;

/// TTF 字体元信息（表偏移/度量）。Copy：首次解析后缓存在 TtfWs.fi，
/// 避免每页翻页重复读表目录 + 重读 cmap 子表。
#[derive(Clone, Copy)]
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
    ascent: i16,
    descent: i16,
}

impl FontInfo {
    /// 行高 = (升部 + |降部|) 缩放 + 1px 间隙。比 px+4 紧，能多排几行铺满屏。
    pub fn line_height(&self, px: f32) -> i32 {
        let h = (self.ascent as i32 - self.descent as i32) as f32 * px / self.units_per_em as f32;
        h as i32 + 1
    }
}

/// TTF 渲染工作区（~18KB）。堆分配，进阅读时创建、退出时释放。
/// 字形缓存 GC_N=96：足够容纳一整页 CJK 的去重字形，使「渲染后预加载下一页」
/// 真正命中缓存——翻页时绝大多数字形直接 blit，不再重新光栅化。
pub struct TtfWs {
    pub pts_x: [i32; MAX_PTS],
    pub pts_y: [i32; MAX_PTS],
    pub pts_on: [u8; MAX_PTS],
    pub end_pts: [u16; MAX_CONTOURS],
    /// 线段池（定点 Q8：坐标=像素×256）。f32 在无 FPU 的 C3 上慢 ~10×，改定点。
    pub seg: [i32; MAX_SEGS * 4],
    pub cross_x: [i32; MAX_CROSS],
    pub cmap_cache: [u8; CMAP_CACHE_MAX],
    pub cmap_len: u32,
    /// 整张 hmtx（水平度量）表缓存进 RAM：advance() 不再每字读 SD，
    /// 翻页时布局零 SD 读（只剩读 .txt 正文一次）。
    pub hmtx_cache: [u8; HMTX_CACHE_MAX],
    pub hmtx_len: u32,
    /// 整张 loca 表缓存进 RAM：parse_glyph 取字形偏移不再每字读 2 次 SD。
    pub loca_cache: [u8; LOCA_CACHE_MAX],
    pub loca_len: u32,
    pub glyf_buf: [u8; GLYF_BUF_MAX],
    pub book_buf: [u8; BOOK_BUF_MAX],
    pub gc_gid: [u16; GC_N],
    pub gc_w: [u8; GC_N],
    pub gc_h: [u8; GC_N],
    /// 该字形位图 top 相对行顶(pen_y)的 y 偏移：基线对齐用（标点/不同升部字下沉到底）。
    pub gc_yoff: [i16; GC_N],
    pub gc_bmp: [[u8; GC_BMP_BYTES]; GC_N],
    pub gc_next: usize,
    /// 缓存已解析的 FontInfo（首次 open_font 填充）；Some 时 open_font 直接返回，不再读 SD。
    pub fi: Option<FontInfo>,
    /// 诊断：本次 paginate_render 的缓存命中/未命中计数（翻页提速排查用）。
    pub dbg_hits: u32,
    pub dbg_miss: u32,
}

/// 堆上分配并清零一个 TtfWs（直接 Layout 分配，避免 Box::new 的栈临时量）。
pub fn alloc_ws() -> Option<&'static mut TtfWs> {
    let layout = Layout::new::<TtfWs>();
    unsafe {
        let ptr = alloc(layout);
        if ptr.is_null() {
            return None;
        }
        core::ptr::write_bytes(ptr, 0, size_of::<TtfWs>());
        Some(&mut *(ptr as *mut TtfWs))
    }
}

/// 释放 TtfWs（退出阅读时调用）。
pub unsafe fn free_ws(ws: *mut TtfWs) {
    let layout = Layout::new::<TtfWs>();
    dealloc(ws as *mut u8, layout);
}

// ── 随机读取（文件，无 ws）──
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
unsafe fn read_into(f: &mut ActualFile, off: u32, dst: *mut u8, len: usize) {
    read_at(f, off, core::slice::from_raw_parts_mut(dst, len));
}

// cmap 缓存读取（ws）
fn cu16(ws: &TtfWs, rel: usize) -> u16 {
    u16::from_be_bytes([ws.cmap_cache[rel], ws.cmap_cache[rel + 1]])
}
fn cu32(ws: &TtfWs, rel: usize) -> u32 {
    u32::from_be_bytes([
        ws.cmap_cache[rel],
        ws.cmap_cache[rel + 1],
        ws.cmap_cache[rel + 2],
        ws.cmap_cache[rel + 3],
    ])
}

// loca 缓存读取（ws）
fn lu16(ws: &TtfWs, o: usize) -> u16 {
    u16::from_be_bytes([ws.loca_cache[o], ws.loca_cache[o + 1]])
}
fn lu32(ws: &TtfWs, o: usize) -> u32 {
    u32::from_be_bytes([
        ws.loca_cache[o],
        ws.loca_cache[o + 1],
        ws.loca_cache[o + 2],
        ws.loca_cache[o + 3],
    ])
}

fn parse_font_info(f: &mut ActualFile, ws: &mut TtfWs) -> Option<FontInfo> {
    let num_tables = u16_at(f, 4);
    let mut off = 12u32;
    let (mut loc, mut glyf, mut hmtx, mut cmap, mut head, mut maxp, mut hhea) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
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
    let ascent = i16_at(f, hhea + 4); // 基线以上的升部（用于行内基线对齐）
    let descent = i16_at(f, hhea + 6); // 基线以下的降部（负值；与升部算行高）
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
    // 整块读 cmap 子表进 ws.cmap_cache
    let sub_len = if cmap_fmt == 12 {
        u32_at(f, cmap_sub + 2) as usize
    } else {
        u16_at(f, cmap_sub + 2) as usize
    };
    if sub_len > 0 && sub_len <= CMAP_CACHE_MAX {
        let mut off_b = 0u32;
        while off_b < sub_len as u32 {
            let n = (sub_len as u32 - off_b).min(512) as usize;
            let mut tmp = [0u8; 512];
            read_at(f, cmap_sub + off_b, &mut tmp[..n]);
            for i in 0..n {
                ws.cmap_cache[off_b as usize + i] = tmp[i];
            }
            off_b += n as u32;
        }
        ws.cmap_len = sub_len as u32;
    }
    // 整块读 hmtx 进 ws.hmtx_cache：advance() 之后从 RAM 取，翻页布局零 SD 读。
    let hmtx_bytes = (num_h_metrics as usize).saturating_mul(4);
    if hmtx_bytes > 0 && hmtx_bytes <= HMTX_CACHE_MAX {
        let mut off_b = 0u32;
        while off_b < hmtx_bytes as u32 {
            let n = (hmtx_bytes as u32 - off_b).min(512) as usize;
            let mut tmp = [0u8; 512];
            read_at(f, hmtx + off_b, &mut tmp[..n]);
            for i in 0..n {
                ws.hmtx_cache[off_b as usize + i] = tmp[i];
            }
            off_b += n as u32;
        }
        ws.hmtx_len = hmtx_bytes as u32;
    }
    // 整块读 loca 进 ws.loca_cache：parse_glyph 取字形偏移不再每字读 2 次 SD。
    let loca_entry = if loc_fmt == 1 {
        (num_glyphs as usize + 1) * 4
    } else {
        (num_glyphs as usize + 1) * 2
    };
    if loca_entry > 0 && loca_entry <= LOCA_CACHE_MAX {
        let mut off_b = 0u32;
        while off_b < loca_entry as u32 {
            let n = (loca_entry as u32 - off_b).min(512) as usize;
            let mut tmp = [0u8; 512];
            read_at(f, loc + off_b, &mut tmp[..n]);
            for i in 0..n {
                ws.loca_cache[off_b as usize + i] = tmp[i];
            }
            off_b += n as u32;
        }
        ws.loca_len = loca_entry as u32;
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
        ascent,
        descent,
    })
}

/// 解析已打开的字体（表目录 + 缓存 cmap 进 ws）。
///
/// **幂等**：首次调用读表目录 + 把整个 cmap 子表缓存进 ws.cmap_cache，并把解析出的
/// FontInfo 存进 ws.fi。后续每次翻页（render / preload 各调一次）直接返回缓存的
/// FontInfo，**不再读 SD**——这是翻页提速的关键（否则每次都要重扫表目录 + 重读 2KB cmap）。
pub fn open_font(f: &mut ActualFile, ws: &mut TtfWs) -> Option<FontInfo> {
    if let Some(fi) = ws.fi {
        return Some(fi);
    }
    let fi = parse_font_info(f, ws)?;
    ws.fi = Some(fi);
    Some(fi)
}

fn char_to_gid(ws: &TtfWs, fi: &FontInfo, ch: char) -> u16 {
    let c = ch as u32;
    if fi.cmap_fmt == 12 {
        let ng = cu32(ws, 12);
        let base = 16usize;
        let mut lo = 0i64;
        let mut hi = ng as i64 - 1;
        while lo <= hi {
            let m = ((lo + hi) / 2) as usize;
            let g = base + m * 12;
            let st = cu32(ws, g);
            let en = cu32(ws, g + 4);
            let sg = cu32(ws, g + 8);
            if c < st {
                hi = m as i64 - 1;
            } else if c > en {
                lo = m as i64 + 1;
            } else {
                return (sg + (c - st)) as u16;
            }
        }
    } else if fi.cmap_fmt == 4 {
        let seg = cu16(ws, 6) as usize / 2;
        let eb = 14usize;
        let sb = eb + seg * 2 + 2;
        let db = sb + seg * 2;
        let rb = db + seg * 2;
        let mut i = 0usize;
        while i < seg {
            if cu16(ws, eb + i * 2) as u32 >= c {
                let start = cu16(ws, sb + i * 2) as u32;
                if start > c {
                    return 0;
                }
                let d = cu16(ws, db + i * 2) as i16 as i32;
                let ro = cu16(ws, rb + i * 2) as u32;
                if ro == 0 {
                    return ((c as i32 + d) & 0xFFFF) as u16;
                }
                let gid_addr = rb + i * 2 + ro as usize + 2 * (c - start) as usize;
                let gid = cu16(ws, gid_addr);
                return if gid == 0 { 0 } else { ((gid as i32 + d) & 0xFFFF) as u16 };
            }
            i += 1;
        }
    }
    0
}

fn advance(f: &mut ActualFile, ws: &TtfWs, fi: &FontInfo, gid: u16) -> u16 {
    let idx = if (gid as u32) < fi.num_h_metrics as u32 {
        gid as u32
    } else if fi.num_h_metrics > 0 {
        fi.num_h_metrics as u32 - 1
    } else {
        return 0;
    };
    let o = idx as usize * 4;
    // 命中 hmtx 缓存则从 RAM 取（翻页布局不再读 SD）；否则回退 SD 随机读。
    if ws.hmtx_len > 0 && o + 2 <= ws.hmtx_len as usize {
        u16::from_be_bytes([ws.hmtx_cache[o], ws.hmtx_cache[o + 1]])
    } else {
        u16_at(f, fi.hmtx_off + idx * 4)
    }
}

fn parse_glyph(
    f: &mut ActualFile, fi: &FontInfo, gid: u16, ws: &mut TtfWs,
) -> Option<(usize, usize, [i16; 4])> {
    let (a, b) = if fi.loc_fmt == 1 {
        let o0 = gid as usize * 4;
        let o1 = o0 + 4;
        if ws.loca_len > 0 && o1 + 4 <= ws.loca_len as usize {
            (lu32(ws, o0), lu32(ws, o1))
        } else {
            (u32_at(f, fi.loca_off + gid as u32 * 4), u32_at(f, fi.loca_off + (gid as u32 + 1) * 4))
        }
    } else {
        let o0 = gid as usize * 2;
        let o1 = o0 + 2;
        if ws.loca_len > 0 && o1 + 2 <= ws.loca_len as usize {
            (lu16(ws, o0) as u32 * 2, lu16(ws, o1) as u32 * 2)
        } else {
            (u16_at(f, fi.loca_off + gid as u32 * 2) as u32 * 2, u16_at(f, fi.loca_off + (gid as u32 + 1) * 2) as u32 * 2)
        }
    };
    let glen = b.saturating_sub(a) as usize;
    if glen == 0 || glen > GLYF_BUF_MAX {
        return None;
    }
    unsafe { read_into(f, fi.glyf_off + a, ws.glyf_buf.as_mut_ptr(), glen); }
    let gu16 = |rel: usize| u16::from_be_bytes([ws.glyf_buf[rel], ws.glyf_buf[rel + 1]]);
    let gi16 = |rel: usize| gu16(rel) as i16;

    let nc = gi16(0);
    if nc < 0 || nc as usize > MAX_CONTOURS {
        return None;
    }
    let nc = nc as usize;
    let bbox = [gi16(2), gi16(4), gi16(6), gi16(8)];
    let mut p = 10usize;
    for i in 0..nc {
        ws.end_pts[i] = gu16(p + i * 2);
    }
    p += nc * 2;
    let npt = if nc > 0 { ws.end_pts[nc - 1] as usize + 1 } else { return None };
    if npt > MAX_PTS {
        return None;
    }
    let instr_len = gu16(p);
    p += 2 + instr_len as usize;
    // flags
    let mut got = 0usize;
    while got < npt {
        let fl = ws.glyf_buf[p];
        p += 1;
        ws.pts_on[got] = fl;
        got += 1;
        if fl & 0x08 != 0 {
            let rep = ws.glyf_buf[p] as usize;
            p += 1;
            for _ in 0..rep {
                if got >= npt { break; }
                ws.pts_on[got] = fl;
                got += 1;
            }
        }
    }
    // x
    let mut x: i32 = 0;
    for i in 0..npt {
        let fl = ws.pts_on[i];
        if fl & 0x02 != 0 {
            let dx = ws.glyf_buf[p] as i32;
            p += 1;
            x += if fl & 0x10 != 0 { dx } else { -dx };
        } else if fl & 0x10 == 0 {
            x += gi16(p) as i32;
            p += 2;
        }
        ws.pts_x[i] = x;
    }
    // y
    let mut y: i32 = 0;
    for i in 0..npt {
        let fl = ws.pts_on[i];
        if fl & 0x04 != 0 {
            let dy = ws.glyf_buf[p] as i32;
            p += 1;
            y += if fl & 0x20 != 0 { dy } else { -dy };
        } else if fl & 0x20 == 0 {
            y += gi16(p) as i32;
            p += 2;
        }
        ws.pts_y[i] = y;
    }
    Some((npt, nc, bbox))
}

fn seg_push(ws: &mut TtfWs, sn: &mut usize, x0: i32, y0: i32, x1: i32, y1: i32) {
    if *sn < MAX_SEGS {
        let b = *sn * 4;
        ws.seg[b] = x0;
        ws.seg[b + 1] = y0;
        ws.seg[b + 2] = x1;
        ws.seg[b + 3] = y1;
        *sn += 1;
    }
}
/// 二次贝塞尔曲线定点化（Q8）：6 等分，系数 (1-t)²,2(1-t)t,t² 预算成 Q8(×256)。
/// B = (c0*p0 + c1*p1 + c2*p2) >> 8。c、p 均为 Q8，乘积 Q16，>>8 回 Q8。全程整数，无 FPU。
fn seg_quad(ws: &mut TtfWs, sn: &mut usize, p0: (i32, i32), p1: (i32, i32), p2: (i32, i32)) {
    const C: [[i32; 3]; 6] = [
        [178, 71, 7], [114, 114, 28], [64, 128, 64], [28, 114, 114], [7, 71, 178], [0, 0, 256],
    ];
    let (mut cx, mut cy) = p0;
    for k in 0..6 {
        let c = C[k];
        let ex = (c[0] * p0.0 + c[1] * p1.0 + c[2] * p2.0) >> 8;
        let ey = (c[0] * p0.1 + c[1] * p1.1 + c[2] * p2.1) >> 8;
        seg_push(ws, sn, cx, cy, ex, ey);
        cx = ex;
        cy = ey;
    }
}

fn flatten_contour(
    ws: &mut TtfWs, mut seg_n: usize, s: usize, e: usize, sc_q8: i32, x0: i32, y1: i32,
) -> usize {
    let n = e - s + 1;
    if n == 0 { return seg_n; }
    // 定点（Q8）：像素坐标 = font_unit * sc_q8，sc_q8 = px*256/upem。中点 /2 → >>1。
    let tx = |xi: i32| (xi - x0) * sc_q8;
    let ty = |yi: i32| (y1 - yi) * sc_q8;

    let mut start = 0usize;
    while start < n {
        if ws.pts_on[s + start] & 1 != 0 { break; }
        start += 1;
    }
    if start == n {
        let mut vcur = (
            (tx(ws.pts_x[s + n - 1]) + tx(ws.pts_x[s])) >> 1,
            (ty(ws.pts_y[s + n - 1]) + ty(ws.pts_y[s])) >> 1,
        );
        for k in 0..n {
            let ctrl = (tx(ws.pts_x[s + k]), ty(ws.pts_y[s + k]));
            let nxt = (tx(ws.pts_x[s + (k + 1) % n]), ty(ws.pts_y[s + (k + 1) % n]));
            let end = ((ctrl.0 + nxt.0) >> 1, (ctrl.1 + nxt.1) >> 1);
            seg_quad(ws, &mut seg_n, vcur, ctrl, end);
            vcur = end;
        }
        return seg_n;
    }
    let idx = |k: usize| s + (start + k) % n;
    let v0 = (tx(ws.pts_x[idx(0)]), ty(ws.pts_y[idx(0)]));
    let mut vcur = v0;
    let mut k = 1usize;
    while k < n {
        let ison = ws.pts_on[idx(k)] & 1 != 0;
        if ison {
            let p = (tx(ws.pts_x[idx(k)]), ty(ws.pts_y[idx(k)]));
            seg_push(ws, &mut seg_n, vcur.0, vcur.1, p.0, p.1);
            vcur = p;
            k += 1;
        } else {
            let ctrl = (tx(ws.pts_x[idx(k)]), ty(ws.pts_y[idx(k)]));
            if k + 1 < n && ws.pts_on[idx(k + 1)] & 1 != 0 {
                let end = (tx(ws.pts_x[idx(k + 1)]), ty(ws.pts_y[idx(k + 1)]));
                seg_quad(ws, &mut seg_n, vcur, ctrl, end);
                vcur = end;
                k += 2;
            } else {
                let nxt = (tx(ws.pts_x[idx(k + 1)]), ty(ws.pts_y[idx(k + 1)]));
                let end = ((ctrl.0 + nxt.0) >> 1, (ctrl.1 + nxt.1) >> 1);
                seg_quad(ws, &mut seg_n, vcur, ctrl, end);
                vcur = end;
                k += 1;
            }
        }
    }
    if vcur != v0 {
        seg_push(ws, &mut seg_n, vcur.0, vcur.1, v0.0, v0.1);
    }
    seg_n
}

fn fill_glyph<D: DrawTarget<Color = BinaryColor>>(
    ws: &mut TtfWs, display: &mut D, seg_n: usize, ox: i32, oy: i32, w: i32, h: i32, color: BinaryColor,
) {
    for y in 0..h {
        let yc = (y << 8) + 128; // Q8: y + 0.5
        let mut nc = 0usize;
        for s in 0..seg_n {
            let b = s * 4;
            let (ax, ay) = (ws.seg[b], ws.seg[b + 1]);
            let (bx, by) = (ws.seg[b + 2], ws.seg[b + 3]);
            if (ay <= yc && yc < by) || (by <= yc && yc < ay) {
                if nc < MAX_CROSS {
                    let dy = by - ay;
                    // t_q8=(yc-ay)/dy ∈[0,256]；cross=ax + t_q8*(bx-ax)/256
                    let t_q8 = if dy != 0 { ((yc - ay) << 8) / dy } else { 0 };
                    ws.cross_x[nc] = ax + ((t_q8 * (bx - ax)) >> 8);
                    nc += 1;
                }
            }
        }
        for i in 1..nc {
            let mut j = i;
            while j > 0 {
                let a = ws.cross_x[j - 1];
                let bv = ws.cross_x[j];
                if a > bv {
                    ws.cross_x[j - 1] = bv;
                    ws.cross_x[j] = a;
                    j -= 1;
                } else { break; }
            }
        }
        let mut k = 0;
        while k + 1 < nc {
            let x0i = (ws.cross_x[k] >> 8).max(0);
            let x1i = (ws.cross_x[k + 1] >> 8).min(w);
            for x in x0i..x1i {
                let _ = display.draw_iter(core::iter::once(Pixel(Point::new(ox + x, oy + y), color)));
            }
            k += 2;
        }
    }
}

// ── 字形位图缓存 ──
fn cache_lookup(ws: &TtfWs, gid: u16) -> Option<usize> {
    let mut i = 0;
    while i < GC_N {
        if ws.gc_gid[i] == gid { return Some(i); }
        i += 1;
    }
    None
}
fn cache_store(ws: &mut TtfWs, gid: u16, w: u8, h: u8, yoff: i16) -> usize {
    let slot = ws.gc_next;
    ws.gc_next = (slot + 1) % GC_N;
    ws.gc_gid[slot] = gid;
    ws.gc_w[slot] = w;
    ws.gc_h[slot] = h;
    ws.gc_yoff[slot] = yoff;
    for i in 0..GC_BMP_BYTES { ws.gc_bmp[slot][i] = 0; }
    slot
}
fn fill_into_bitmap(ws: &mut TtfWs, seg_n: usize, gw: i32, gh: i32, slot: usize) {
    let rowbytes = ((gw as usize) + 7) / 8;
    for y in 0..gh {
        let yc = (y << 8) + 128; // Q8: y + 0.5
        let mut nc = 0usize;
        let mut s = 0;
        while s < seg_n {
            let b = s * 4;
            let (ax, ay) = (ws.seg[b], ws.seg[b + 1]);
            let (bx, by) = (ws.seg[b + 2], ws.seg[b + 3]);
            if (ay <= yc && yc < by) || (by <= yc && yc < ay) {
                if nc < MAX_CROSS {
                    let dy = by - ay;
                    let t_q8 = if dy != 0 { ((yc - ay) << 8) / dy } else { 0 };
                    ws.cross_x[nc] = ax + ((t_q8 * (bx - ax)) >> 8);
                    nc += 1;
                }
            }
            s += 1;
        }
        let mut i = 1;
        while i < nc {
            let mut j = i;
            while j > 0 {
                let a = ws.cross_x[j - 1];
                let bv = ws.cross_x[j];
                if a > bv { ws.cross_x[j - 1] = bv; ws.cross_x[j] = a; j -= 1; } else { break; }
            }
            i += 1;
        }
        let mut k = 0;
        while k + 1 < nc {
            let mut x = (ws.cross_x[k] >> 8).max(0);
            let xend = (ws.cross_x[k + 1] >> 8).min(gw);
            while x < xend {
                if x >= 0 && (x as usize) < gw as usize {
                    let idx = (y as usize) * rowbytes + (x as usize) / 8;
                    let mask = 0x80u8 >> (x % 8);
                    ws.gc_bmp[slot][idx] |= mask;
                }
                x += 1;
            }
            k += 2;
        }
    }
}
fn blit_slot<D: DrawTarget<Color = BinaryColor>>(ws: &TtfWs, display: &mut D, slot: usize, ox: i32, oy: i32) {
    let w = ws.gc_w[slot] as i32;
    let h = ws.gc_h[slot] as i32;
    let rowbytes = ((w as usize) + 7) / 8;
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let idx = (y as usize) * rowbytes + (x as usize) / 8;
            if ws.gc_bmp[slot][idx] & (0x80u8 >> (x % 8)) != 0 {
                let _ = display.draw_iter(core::iter::once(Pixel(Point::new(ox + x, oy + y), BinaryColor::On)));
            }
            x += 1;
        }
    }
}

/// 读 .txt 一页字节进 ws.book_buf，返回读到的字节数。
pub fn read_book_chunk(f: &mut ActualFile, offset: u32, ws: &mut TtfWs) -> usize {
    let _ = f.seek_from_start(offset);
    let mut got = 0usize;
    while got < BOOK_BUF_MAX {
        match f.read(&mut ws.book_buf[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    got
}

/// 即时分页+渲染：扫描 ws.book_buf[..book_len] 的 UTF-8，按 advance 换行、满 max_lines 止，
/// 返回消费字节数。字形命中缓存则 blit，否则光栅化进缓存再 blit。
pub fn paginate_render<D: DrawTarget<Color = BinaryColor>>(
    f: &mut ActualFile, fi: &FontInfo, ws: &mut TtfWs, display: &mut D,
    book_len: usize, px: f32, top_left: Point, max_w: i32, line_h: i32, max_lines: u32,
) -> usize {
    if max_lines == 0 { return 0; }
    ws.dbg_hits = 0;
    ws.dbg_miss = 0;
    let sc = px / fi.units_per_em as f32; // advance 布局用（每字一次，f32 可接受）
    let sc_q8 = (px * 256.0 / fi.units_per_em as f32) as i32; // 光栅化定点（无 FPU 提速）
    let left = top_left.x;
    let mut pen_x = left;
    let mut pen_y = top_left.y;
    let mut line = 0u32;
    let mut i = 0usize;
    while i < book_len {
        let b0 = ws.book_buf[i];
        let clen = if b0 < 0x80 { 1 } else if b0 < 0xC0 { 1 }
            else if b0 < 0xE0 { 2 } else if b0 < 0xF0 { 3 } else { 4 };
        if i + clen > book_len { break; }
        let ch = core::str::from_utf8(&ws.book_buf[i..i + clen])
            .ok().and_then(|s| s.chars().next()).unwrap_or('\u{FFFD}');
        if ch == '\n' || ch == '\r' {
            line += 1;
            i += clen;
            if line >= max_lines { break; }
            pen_x = left;
            pen_y += line_h;
            continue;
        }
        let gid = char_to_gid(ws, fi, ch);
        let adv_f = advance(f, ws, fi, gid) as f32 * sc;
        if pen_x + adv_f as i32 > left + max_w {
            line += 1;
            if line >= max_lines { break; }
            pen_x = left;
            pen_y += line_h;
        }
        if let Some(slot) = cache_lookup(ws, gid) {
            ws.dbg_hits += 1;
            blit_slot(ws, display, slot, pen_x, pen_y + ws.gc_yoff[slot] as i32);
        } else if let Some((npt, ncon, bbox)) = parse_glyph(f, fi, gid, ws) {
            ws.dbg_miss += 1;
            let x0 = bbox[0] as i32;
            let x1 = bbox[2] as i32;
            let ymax = bbox[3] as i32;
            // 基线对齐：位图 top 相对行顶的偏移 = (升部 - 字形顶) 缩放。
            // CJK（顶≈升部）→ 偏移≈0（贴行顶）；标点（顶小）→ 下沉到基线。
            let yoff = (((fi.ascent as i32 - ymax) * sc_q8) >> 8) as i16;
            let gw = ((x1 - x0) * sc_q8 >> 8) + 1;
            let gh = ((bbox[3] as i32 - bbox[1] as i32) * sc_q8 >> 8) + 1;
            let mut seg_n = 0usize;
            let mut si = 0usize;
            for ci in 0..ncon {
                let ei = ws.end_pts[ci] as usize;
                if ei >= npt { break; }
                seg_n = flatten_contour(ws, seg_n, si, ei, sc_q8, x0, ymax);
                si = ei + 1;
            }
            let rowbytes = ((gw as usize) + 7) / 8;
            if gw > 0 && gh > 0 && (gh as usize) * rowbytes <= GC_BMP_BYTES {
                let slot = cache_store(ws, gid, gw as u8, gh as u8, yoff);
                fill_into_bitmap(ws, seg_n, gw, gh, slot);
                blit_slot(ws, display, slot, pen_x, pen_y + yoff as i32);
            } else {
                fill_glyph(ws, display, seg_n, pen_x, pen_y + yoff as i32, gw, gh, BinaryColor::On);
            }
        }
        pen_x += adv_f as i32;
        i += clen;
    }
    i
}

/// 预加载：扫描 ws.book_buf[..book_len] 的字形，**只缓存不画**（加速下一次翻页）。
/// 逻辑同 paginate_render，但跳过所有 display 绘制。
///
/// **async + 周期让出 CPU**：光栅化是软浮点重活（~10ms/字），一页 ~170 字要 ~1.8s。
/// 若同步跑会卡死显示任务的墨水屏刷新（协作式调度，日志里 preload 堵在 begin render 之前）。
/// 每 8 个字 await 一次，让显示任务能并行刷屏——翻页时屏幕先出图，预加载在后台跑完。
pub async fn preload_glyphs(
    f: &mut ActualFile<'_>, fi: &FontInfo, ws: &mut TtfWs,
    book_len: usize, px: f32, max_w: i32, line_h: i32, max_lines: u32,
) {
    if max_lines == 0 { return; }
    let sc = px / fi.units_per_em as f32;
    let sc_q8 = (px * 256.0 / fi.units_per_em as f32) as i32;
    let mut pen_x = 0i32;
    let mut line = 0u32;
    let mut i = 0usize;
    let mut since_yield = 0u32;
    while i < book_len {
        let b0 = ws.book_buf[i];
        let clen = if b0 < 0x80 { 1 } else if b0 < 0xC0 { 1 }
            else if b0 < 0xE0 { 2 } else if b0 < 0xF0 { 3 } else { 4 };
        if i + clen > book_len { break; }
        let ch = core::str::from_utf8(&ws.book_buf[i..i + clen])
            .ok().and_then(|s| s.chars().next()).unwrap_or('\u{FFFD}');
        if ch == '\n' || ch == '\r' {
            line += 1; i += clen;
            if line >= max_lines { break; }
            pen_x = 0;
            continue;
        }
        let gid = char_to_gid(ws, fi, ch);
        let adv_f = advance(f, ws, fi, gid) as f32 * sc;
        if pen_x + adv_f as i32 > max_w {
            line += 1;
            if line >= max_lines { break; }
            pen_x = 0;
        }
        // 只缓存不画
        if cache_lookup(ws, gid).is_none() {
            if let Some((npt, ncon, bbox)) = parse_glyph(f, fi, gid, ws) {
                let x0 = bbox[0] as i32;
                let x1 = bbox[2] as i32;
                let ymax = bbox[3] as i32;
                let yoff = (((fi.ascent as i32 - ymax) * sc_q8) >> 8) as i16;
                let gw = ((x1 - x0) * sc_q8 >> 8) + 1;
                let gh = ((bbox[3] as i32 - bbox[1] as i32) * sc_q8 >> 8) + 1;
                let mut seg_n = 0usize;
                let mut si = 0usize;
                for ci in 0..ncon {
                    let ei = ws.end_pts[ci] as usize;
                    if ei >= npt { break; }
                    seg_n = flatten_contour(ws, seg_n, si, ei, sc_q8, x0, ymax);
                    si = ei + 1;
                }
                let rowbytes = ((gw as usize) + 7) / 8;
                if gw > 0 && gh > 0 && (gh as usize) * rowbytes <= GC_BMP_BYTES {
                    let slot = cache_store(ws, gid, gw as u8, gh as u8, yoff);
                    fill_into_bitmap(ws, seg_n, gw, gh, slot);
                }
            }
        }
        pen_x += adv_f as i32;
        i += clen;
        // 周期让出 CPU：让显示任务能并行刷墨水屏（preload 不再阻塞刷新）
        since_yield += 1;
        if since_yield >= 8 {
            since_yield = 0;
            embassy_time::Timer::after(embassy_time::Duration::from_micros(1)).await;
        }
    }
}
