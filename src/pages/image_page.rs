use alloc::boxed::Box;
use alloc::format;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::prelude::Primitive;
use epd_waveshare::color::{Black, Color, White};
use epd_waveshare::prelude::Display;
use epd_waveshare::graphics::DisplayRotation;
use esp_println::println;
use heapless::{String, Vec};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::{display, event};
use crate::event::EventType;
use crate::pages::Page;
use crate::sd_mount::{ActualDirectory, SD_MOUNT, SdMount, BOOK_NAME_MAX};
use crate::sleep::{refresh_active_time, to_sleep_tips};
use crate::widgets::list_widget::ListWidget;

// Physical display: 400x300, buffer row stride = 50 bytes
const PHYS_W: u32 = 400;
const PHYS_H: u32 = 300;
const BUF_ROW: usize = (PHYS_W as usize + 7) / 8; // 50
// Visual (Rotate90): 300x400
const VIS_W: u32 = PHYS_H; // 300
const VIS_H: u32 = PHYS_W; // 400
// Max source row buffer on stack (supports up to 512px wide at 32bpp)
const SRC_ROW_BUF: usize = 2048;

const IMAGE_MENU_ITEMS: &[&str] = &["返回列表", "设置为壁纸"];
const LIST_EXIT_LABEL: &str = "退出";

enum ImageMenuState {
    Closed,
    Popup { menu_index: u32 },
}

pub struct ImagePage {
    running: bool,
    viewing: bool,
    loading: bool,
    choose_index: u32,
    menus: Option<Vec<String<BOOK_NAME_MAX>, 40>>,
    need_render: bool,
    menu_state: ImageMenuState,
    save_wallpaper_flag: bool,
    status_msg: Option<&'static str>,
}

/// Read exactly `n` bytes from file into buf. Returns false on EOF or error.
fn read_exact(file: &mut crate::sd_mount::ActualFile, buf: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(n) if n > 0 => filled += n,
            _ => return false,
        }
    }
    true
}

impl ImagePage {
    async fn back(&mut self) {
        self.running = false;
    }

    /// Get the actual image index (0-based in menus), accounting for "退出" at index 0.
    /// Returns None if choose_index points to "退出" or is out of range.
    fn image_index(&self) -> Option<usize> {
        if self.choose_index == 0 { return None; }
        let idx = (self.choose_index - 1) as usize;
        self.menus.as_ref().and_then(|m| if idx < m.len() { Some(idx) } else { None })
    }

    /// Convert one BMP source row to packed 1-bit and write directly into the
    /// display buffer at visual row `dy`. The display is in Rotate90 mode.
    ///
    /// Buffer layout for Rotate90: visual(vx,vy) → physical(W-1-vy, vx)
    /// So visual row vy writes bit into physical column W-1-vy across all physical rows 0..V.
    /// Equivalently: for each vx (0..VIS_W), set/clear bit at buffer row vx, byte (W-1-vy)/8.
    fn write_visual_row(buf: &mut [u8], dy: u32, src_row: &[u8], info: &crate::flash_sleep::BmpInfo) {
        let row_bytes = (VIS_W as usize + 7) / 8;
        let mut pixel_row = [0u8; 40]; // VIS_W=300 → 38 bytes, 40 is safe
        for b in pixel_row[..row_bytes].iter_mut() { *b = 0xFF; } // default white

        // Convert source row to packed 1-bit (1=white, 0=black in display buffer)
        for dx in 0..VIS_W {
            let sx = dx * info.bmp_w / VIS_W;
            let is_black = match info.bpp {
                1 => {
                    let byte_idx = sx as usize / 8;
                    let bit_idx = 7 - (sx % 8);
                    byte_idx < src_row.len() && (src_row[byte_idx] >> bit_idx) & 1 == 0
                }
                24 => {
                    let px = sx as usize * 3;
                    if px + 2 < src_row.len() {
                        let (b, g, r) = (src_row[px] as u32, src_row[px+1] as u32, src_row[px+2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else { false }
                }
                32 => {
                    let px = sx as usize * 4;
                    if px + 2 < src_row.len() {
                        let (b, g, r) = (src_row[px] as u32, src_row[px+1] as u32, src_row[px+2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else { false }
                }
                _ => false,
            };
            if is_black {
                // In display buffer: 0 = black, so clear the bit
                pixel_row[dx as usize / 8] &= !(1u8 << (7 - (dx as usize % 8)));
            }
        }

        // Write pixel_row to display buffer using Rotate90 mapping:
        // visual(vx, dy) → physical(PHYS_W-1-dy, vx)
        // buffer offset = vx * BUF_ROW + (PHYS_W-1-dy)/8
        // bit = 0x80 >> ((PHYS_W-1-dy) % 8)
        let phys_byte = (PHYS_W - 1 - dy) as usize / 8;
        let phys_bit: u8 = 0x80 >> ((PHYS_W - 1 - dy) as usize % 8);
        for vx in 0..VIS_W as usize {
            let px_byte = vx / 8;
            let px_bit = 7 - (vx % 8);
            let val = (pixel_row[px_byte] >> px_bit) & 1;
            let buf_idx = vx * BUF_ROW + phys_byte;
            if val == 0 {
                // black
                buf[buf_idx] &= !phys_bit;
            }
            // white bit already set by initial 0xFF fill
        }
    }

    /// Stream-read BMP from SD and draw directly into the provided buffer.
    /// Caller must ensure buf is filled with 0xFF (white) before calling.
    /// Returns false on failure.
    fn draw_image_to_buffer(&mut self, buf: &mut [u8], images_dir: &mut ActualDirectory<'_>) -> bool {
        let image_name = match self.image_index().and_then(|i| self.menus.as_ref().map(|m| m[i].clone())) {
            Some(n) => n,
            None => return false,
        };

        let file_name = format!("{}.bmp", image_name);
        println!("[img] opening: {}", file_name);
        let file_result = SdMount::open_file_by_name(images_dir, &file_name, embedded_sdmmc::Mode::ReadOnly);
        let mut file = match file_result {
            Ok(f) => f,
            Err(e) => { println!("[img] open error: {:?}", e); return false; }
        };

        // Read BMP header
        let mut hdr = [0u8; 54];
        if !read_exact(&mut file, &mut hdr) {
            println!("[img] header read failed");
            file.close();
            return false;
        }
        println!("[img] header read ok");

        let info = match crate::flash_sleep::BmpInfo::parse(&hdr) {
            Some(i) => i,
            None => { println!("[img] invalid BMP header"); file.close(); return false; }
        };
        println!("[img] BMP {}x{} bpp={} offset={} stride={} top_down={}",
            info.bmp_w, info.bmp_h, info.bpp, info.pixel_offset, info.src_row_stride, info.top_down);

        if info.src_row_stride > SRC_ROW_BUF {
            println!("[img] row too wide: {} bytes", info.src_row_stride);
            file.close();
            return false;
        }

        // Skip to pixel data (past any palette or gap between header and pixels)
        let hdr_end = 54;
        if info.pixel_offset > hdr_end {
            let skip = info.pixel_offset - hdr_end;
            println!("[img] skipping {} bytes to pixel data", skip);
            let mut skip_buf = [0u8; 512];
            let mut remaining = skip;
            while remaining > 0 {
                let to_read = remaining.min(skip_buf.len());
                if !read_exact(&mut file, &mut skip_buf[..to_read]) {
                    println!("[img] skip read failed");
                    file.close();
                    return false;
                }
                remaining -= to_read;
            }
        }
        println!("[img] pixel data ready, reading rows...");

        let mut row_buf = [0u8; SRC_ROW_BUF];

        // Read source rows sequentially and draw
        for src_row_idx in 0..info.bmp_h {
            if !read_exact(&mut file, &mut row_buf[..info.src_row_stride]) {
                println!("[img] row {} read failed, stopping", src_row_idx);
                break;
            }

            let visual_row = if info.top_down { src_row_idx } else { info.bmp_h - 1 - src_row_idx };

            // Check which target rows map to this visual row
            for dy in 0..VIS_H {
                if dy * info.bmp_h / VIS_H == visual_row {
                    Self::write_visual_row(buf, dy, &row_buf[..info.src_row_stride], &info);
                }
            }

            if src_row_idx % 100 == 0 {
                println!("[img] row {}/{}", src_row_idx, info.bmp_h);
            }
        }

        println!("[img] done, closing file");
        file.close();
        true
    }

    /// Show "处理中..." on screen, render it, then save wallpaper.
    async fn save_wallpaper_with_prompt(&mut self, images_dir: &mut ActualDirectory<'_>) {
        // Show processing indicator
        if let Some(display) = display_mut() {
            let _ = display.clear_buffer(Color::White);
            let font = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
            let _ = font.render_aligned(
                "处理中...",
                Point::new(VIS_W as i32 / 2, VIS_H as i32 / 2),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
        Timer::after(Duration::from_millis(500)).await;

        self.save_wallpaper(images_dir).await;

        // Show result
        if let Some(display) = display_mut() {
            let _ = display.clear_buffer(Color::White);
            let msg = self.status_msg.unwrap_or("壁纸设置失败");
            let font = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
            let _ = font.render_aligned(
                msg,
                Point::new(VIS_W as i32 / 2, VIS_H as i32 / 2),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
        Timer::after(Duration::from_millis(1500)).await;
        self.status_msg = None;
    }

    /// Save current image as wallpaper by re-reading from SD and writing to flash row by row.
    async fn save_wallpaper(&mut self, images_dir: &mut ActualDirectory<'_>) {
        let image_name = match self.image_index().and_then(|i| self.menus.as_ref().map(|m| m[i].clone())) {
            Some(n) => n,
            None => { self.status_msg = Some("壁纸设置失败"); return; }
        };

        let file_name = format!("{}.bmp", image_name);
        let file_result = SdMount::open_file_by_name(images_dir, &file_name, embedded_sdmmc::Mode::ReadOnly);
        let mut file = match file_result {
            Ok(f) => f,
            Err(_) => { self.status_msg = Some("壁纸设置失败"); return; }
        };

        let mut hdr = [0u8; 54];
        if !read_exact(&mut file, &mut hdr) { file.close(); self.status_msg = Some("壁纸设置失败"); return; }
        let info = match crate::flash_sleep::BmpInfo::parse(&hdr) {
            Some(i) => i,
            None => { file.close(); self.status_msg = Some("壁纸设置失败"); return; }
        };
        if info.src_row_stride > SRC_ROW_BUF { file.close(); self.status_msg = Some("壁纸设置失败"); return; }

        // Skip to pixel data
        let hdr_end = 54;
        if info.pixel_offset > hdr_end {
            let mut skip_buf = [0u8; 512];
            let mut remaining = info.pixel_offset - hdr_end;
            while remaining > 0 {
                let to_read = remaining.min(skip_buf.len());
                if !read_exact(&mut file, &mut skip_buf[..to_read]) { file.close(); self.status_msg = Some("壁纸设置失败"); return; }
                remaining -= to_read;
            }
        }

        if crate::flash_sleep::begin_sleep_image_write().is_err() {
            file.close(); self.status_msg = Some("壁纸设置失败"); return;
        }

        let mut row_buf = [0u8; SRC_ROW_BUF];
        let mut pixel_row = [0xFFu8; 40]; // VIS_W=300 → 38 bytes

        for src_row_idx in 0..info.bmp_h {
            if !read_exact(&mut file, &mut row_buf[..info.src_row_stride]) { break; }

            let visual_row = if info.top_down { src_row_idx } else { info.bmp_h - 1 - src_row_idx };

            for dy in 0..VIS_H {
                if dy * info.bmp_h / VIS_H == visual_row {
                    // Convert to packed 1-bit (flash format: bit=1 means black)
                    let row_bytes = (VIS_W as usize + 7) / 8;
                    for b in pixel_row[..row_bytes].iter_mut() { *b = 0; }
                    for dx in 0..VIS_W {
                        let sx = dx * info.bmp_w / VIS_W;
                        let is_black = match info.bpp {
                            1 => {
                                let bi = sx as usize / 8;
                                let bit = 7 - (sx % 8);
                                bi < row_buf.len() && (row_buf[bi] >> bit) & 1 == 0
                            }
                            24 => {
                                let px = sx as usize * 3;
                                if px + 2 < row_buf.len() {
                                    let (b, g, r) = (row_buf[px] as u32, row_buf[px+1] as u32, row_buf[px+2] as u32);
                                    (r * 299 + g * 587 + b * 114) / 1000 < 128
                                } else { false }
                            }
                            32 => {
                                let px = sx as usize * 4;
                                if px + 2 < row_buf.len() {
                                    let (b, g, r) = (row_buf[px] as u32, row_buf[px+1] as u32, row_buf[px+2] as u32);
                                    (r * 299 + g * 587 + b * 114) / 1000 < 128
                                } else { false }
                            }
                            _ => false,
                        };
                        if is_black {
                            pixel_row[dx as usize / 8] |= 1 << (7 - (dx as usize % 8));
                        }
                    }
                    let flash_row_bytes = (VIS_W as usize + 7) / 8;
                    if crate::flash_sleep::write_sleep_pixel_row(dy, &pixel_row[..flash_row_bytes]).is_err() {
                        file.close(); self.status_msg = Some("壁纸设置失败"); return;
                    }
                }
            }
        }

        file.close();
        if crate::flash_sleep::finish_sleep_image_write().is_ok() {
            self.status_msg = Some("壁纸设置成功");
        } else {
            self.status_msg = Some("壁纸设置失败");
        }
    }
}

fn image_sleep_renderer(_display: &mut crate::display::EpdDisplay) {
    //保持图片不绘制
}

impl Page for ImagePage {
    fn new() -> Self {
        Self {
            running: false,
            viewing: false,
            loading: false,
            choose_index: 0,
            menus: None,
            need_render: true,
            menu_state: ImageMenuState::Closed,
            save_wallpaper_flag: false,
            status_msg: None,
        }
    }

    async fn render(&mut self) {
        // List mode rendering handled in run()
    }

    async fn run(&mut self, _spawner: Spawner) {
        display::set_sleep_renderer(Some(image_sleep_renderer));
        if let Some(display) = display_mut() {
            display.set_rotation(DisplayRotation::Rotate90);
        }
        self.running = true;
        self.need_render = true;

        if let Some(ref mut sd) = *SD_MOUNT.lock().await {
            let volume0 = sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0));
            match volume0 {
                Ok(v) => {
                    let root_result = v.open_root_dir();
                    match root_result {
                        Ok(root) => {
                            let images_dir_res = root.open_dir("images");
                            match images_dir_res {
                                Ok(mut images_dir) => {
                                    match SdMount::get_images(&mut images_dir) {
                                        Ok(images) => self.menus = Some(images),
                                        Err(e) => {
                                            println!("get_images error: {:?}", e);
                                            self.menus = Some(Vec::new());
                                        }
                                    }

                                    loop {
                                        if !self.running { break; }

                                        // Handle wallpaper save
                                        if self.save_wallpaper_flag {
                                            self.save_wallpaper_flag = false;
                                            self.save_wallpaper_with_prompt(&mut images_dir).await;
                                            self.viewing = false;
                                            self.menu_state = ImageMenuState::Closed;
                                            self.need_render = true;
                                        }

                                        if self.need_render {
                                            self.need_render = false;

                                            if self.viewing && self.loading {
                                                println!("[img] showing loading screen");
                                                // First pass: show loading indicator
                                                if let Some(display) = display_mut() {
                                                    let _ = display.clear_buffer(Color::White);
                                                    let font = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                                                    let _ = font.render_aligned(
                                                        "加载中...",
                                                        Point::new(VIS_W as i32 / 2, VIS_H as i32 / 2),
                                                        VerticalPosition::Center,
                                                        HorizontalAlignment::Center,
                                                        FontColor::Transparent(Black),
                                                        display,
                                                    );
                                                }
                                                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
                                                Timer::after(Duration::from_millis(500)).await;
                                                self.loading = false;
                                                self.need_render = true;
                                                continue;
                                            }

                                            if let Some(display) = display_mut() {
                                                if self.viewing {
                                                    println!("[img] begin draw_image_to_buffer");
                                                    let ok = {
                                                        display.clear_buffer(Color::White);
                                                        let buf = display.get_mut_buffer();
                                                        self.draw_image_to_buffer(buf, &mut images_dir)
                                                    };
                                                    if !ok {
                                                        let _ = display.clear_buffer(Color::White);
                                                        let font = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                                                        let _ = font.render_aligned(
                                                            "图片格式不支持",
                                                            Point::new(VIS_W as i32 / 2, VIS_H as i32 / 2),
                                                            VerticalPosition::Center,
                                                            HorizontalAlignment::Center,
                                                            FontColor::Transparent(Black),
                                                            display,
                                                        );
                                                    }
                                                    // Menu overlay
                                                    if let ImageMenuState::Popup { menu_index } = self.menu_state {
                                                        self.render_menu_overlay(display, menu_index);
                                                    }
                                                } else {
                                                    // List mode
                                                    let _ = display.clear_buffer(Color::White);
                                                    if let Some(ref menus) = self.menus {
                                                        if menus.is_empty() {
                                                            let font = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                                                            let _ = font.render_aligned(
                                                                "无图片文件\n请将BMP放入SD卡images目录",
                                                                Point::new(VIS_W as i32 / 2, VIS_H as i32 / 2),
                                                                VerticalPosition::Center,
                                                                HorizontalAlignment::Center,
                                                                FontColor::Transparent(Black),
                                                                display,
                                                            );
                                                        } else {
                                                            let mut all_items: Vec<&str, 20> = Vec::new();
                                                            let _ = all_items.push(LIST_EXIT_LABEL);
                                                            for item in menus.iter() {
                                                                if all_items.push(item.as_str()).is_err() { break; }
                                                            }
                                                            let mut list_widget = ListWidget::new(
                                                                Point::new(0, 0), Black, White,
                                                                Size::new(VIS_W, VIS_H), all_items,
                                                            );
                                                            list_widget.choose(self.choose_index as usize);
                                                            let _ = list_widget.draw(display);
                                                        }
                                                    }
                                                    if let Some(msg) = self.status_msg {
                                                        self.status_msg = None;
                                                        let font = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                                                        let _ = font.render_aligned(
                                                            msg,
                                                            Point::new(VIS_W as i32 / 2, VIS_H as i32 - 20),
                                                            VerticalPosition::Center,
                                                            HorizontalAlignment::Center,
                                                            FontColor::Transparent(Black),
                                                            display,
                                                        );
                                                    }
                                                }
                                                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                                                println!("[img] render sent");
                                                refresh_active_time().await;
                                            }
                                        }

                                        //保持图片显示
                                        if self.viewing {
                                            display::set_sleep_renderer(Some(image_sleep_renderer));
                                        }else{
                                            display::set_sleep_renderer(None);
                                        }
                                        to_sleep_tips(Duration::from_secs(0), Duration::from_secs(20), true).await;
                                        Timer::after(Duration::from_millis(100)).await;
                                    }
                                }
                                Err(e) => {
                                    println!("images dir open error: {:?}", e);
                                    self.menus = Some(Vec::new());
                                    if let Some(display) = display_mut() {
                                        display.clear_buffer(Color::White);
                                        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                                    }
                                    loop {
                                        if !self.running { break; }
                                        Timer::after(Duration::from_millis(50)).await;
                                    }
                                }
                            }
                        }
                        Err(e) => println!("root dir open error: {:?}", e),
                    }
                }
                Err(e) => println!("volume open error: {:?}", e),
            }
        }

        display::set_sleep_renderer(None);
        if let Some(display) = display_mut() {
            display.set_rotation(DisplayRotation::Rotate0);
        }
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        let self_ptr = Self::mut_to_ptr(self);

        // Key3 long: exit list or return from view
        event::on_target(EventType::KeyLongEnd(3), self_ptr, move |info| {
            Box::pin(async move {
                let r: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if r.viewing {
                    r.viewing = false;
                    r.menu_state = ImageMenuState::Closed;
                    r.need_render = true;
                    refresh_active_time().await;
                } else {
                    r.back().await;
                }
            })
        }).await;

        // Key3 short
        event::on_target(EventType::KeyShort(3), self_ptr, move |info| {
            Box::pin(async move {
                let r: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                refresh_active_time().await;
                match r.menu_state {
                    ImageMenuState::Popup { menu_index } => match menu_index {
                        0 => {
                            r.viewing = false;
                            r.menu_state = ImageMenuState::Closed;
                            r.need_render = true;
                        }
                        _ => { r.save_wallpaper_flag = true; }
                    },
                    ImageMenuState::Closed => {
                        if r.viewing {
                            r.menu_state = ImageMenuState::Popup { menu_index: 0 };
                            r.need_render = true;
                        } else if let Some(ref menus) = r.menus {
                            if r.choose_index == 0 {
                                r.back().await;
                            } else if (r.choose_index as usize) <= menus.len() {
                                r.viewing = true;
                                r.loading = true;
                                r.menu_state = ImageMenuState::Closed;
                                r.need_render = true;
                            }
                        }
                    }
                }
            })
        }).await;

        // Key1 short: scroll down
        event::on_target(EventType::KeyShort(1), self_ptr, move |info| {
            Box::pin(async move {
                let r: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                refresh_active_time().await;
                match r.menu_state {
                    ImageMenuState::Popup { ref mut menu_index } => {
                        if *menu_index < (IMAGE_MENU_ITEMS.len() - 1) as u32 {
                            *menu_index += 1;
                            r.need_render = true;
                        }
                    }
                    ImageMenuState::Closed if !r.viewing => {
                        let max = r.menus.as_ref().map(|m| m.len() + 1).unwrap_or(1);
                        if r.choose_index < (max - 1) as u32 {
                            r.choose_index += 1;
                            r.need_render = true;
                        }
                    }
                    _ => {}
                }
            })
        }).await;

        event::on_target(EventType::KeyLongIng(1), self_ptr, move |info| {
            Box::pin(async move {
                let r: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match r.menu_state {
                    ImageMenuState::Popup { ref mut menu_index } => {
                        *menu_index = if *menu_index < (IMAGE_MENU_ITEMS.len() - 1) as u32 { *menu_index + 1 } else { 0 };
                        r.need_render = true;
                        Timer::after_millis(200).await;
                    }
                    ImageMenuState::Closed if !r.viewing => {
                        let max = r.menus.as_ref().map(|m| m.len() + 1).unwrap_or(1);
                        if r.choose_index < (max - 1) as u32 {
                            r.choose_index += 1;
                            r.need_render = true;
                        }
                        Timer::after_millis(200).await;
                    }
                    _ => {}
                }
            })
        }).await;

        // Key2 short: scroll up
        event::on_target(EventType::KeyShort(2), self_ptr, move |info| {
            Box::pin(async move {
                let r: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                refresh_active_time().await;
                match r.menu_state {
                    ImageMenuState::Popup { ref mut menu_index } => {
                        if *menu_index > 0 {
                            *menu_index -= 1;
                            r.need_render = true;
                        }
                    }
                    ImageMenuState::Closed if !r.viewing => {
                        if r.choose_index > 0 {
                            r.choose_index -= 1;
                            r.need_render = true;
                        }
                    }
                    _ => {}
                }
            })
        }).await;

        event::on_target(EventType::KeyLongIng(2), self_ptr, move |info| {
            Box::pin(async move {
                let r: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match r.menu_state {
                    ImageMenuState::Popup { ref mut menu_index } => {
                        *menu_index = if *menu_index > 0 { *menu_index - 1 } else { (IMAGE_MENU_ITEMS.len() - 1) as u32 };
                        r.need_render = true;
                        Timer::after_millis(200).await;
                    }
                    ImageMenuState::Closed if !r.viewing => {
                        if r.choose_index > 0 {
                            r.choose_index -= 1;
                            r.need_render = true;
                        }
                        Timer::after_millis(200).await;
                    }
                    _ => {}
                }
            })
        }).await;
    }
}

impl ImagePage {
    fn render_menu_overlay(&self, display: &mut crate::display::EpdDisplay, menu_index: u32) {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let menu_width: u32 = 160;
        let menu_item_height: u32 = 28;
        let menu_padding: u32 = 8;
        let menu_height = IMAGE_MENU_ITEMS.len() as u32 * menu_item_height + menu_padding * 2;

        let menu_x = ((VIS_W - menu_width) / 2) as i32;
        let menu_y = ((VIS_H - menu_height) / 2) as i32;

        let rect = Rectangle::new(Point::new(menu_x, menu_y), Size::new(menu_width, menu_height));
        let style = PrimitiveStyleBuilder::new()
            .fill_color(White).stroke_color(Black)
            .stroke_alignment(StrokeAlignment::Outside).stroke_width(2).build();
        rect.into_styled(style).draw(display).ok();

        for (i, label) in IMAGE_MENU_ITEMS.iter().enumerate() {
            let item_y = menu_y + menu_padding as i32 + (i as u32 * menu_item_height) as i32;
            let is_selected = i as u32 == menu_index;
            if is_selected {
                Rectangle::new(Point::new(menu_x + 4, item_y), Size::new(menu_width - 8, menu_item_height))
                    .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                    .draw(display).ok();
            }
            let prefix = if is_selected { "> " } else { "  " };
            let text_color = if is_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
            font.render_aligned(
                format_args!("{}{}", prefix, label),
                Point::new(menu_x + menu_padding as i32, item_y + menu_item_height as i32 / 2),
                VerticalPosition::Center, HorizontalAlignment::Left, text_color, display,
            ).ok();
        }
    }
}
