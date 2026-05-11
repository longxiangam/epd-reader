use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec as AllocVec;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{Dimensions, Point, Size};
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::prelude::Primitive;
use embedded_sdmmc::ShortFileName;
use epd_waveshare::color::{Black, Color,White};
use epd_waveshare::graphics::{Display, DisplayRotation};
use esp_hal::macros::ram;
use esp_hal::reset::get_reset_reason;
use esp_hal::rtc_cntl::SocResetReason;
use esp_println::{print, println};
use futures::FutureExt;
use heapless::{String, Vec};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::{display, epd2in9_txt, event};
use crate::epd2in9_txt::{BookPages, TxtReader};
use crate::event::EventType;
use crate::pages::{ Page};
use crate::sd_mount::{ActualDirectory, SD_MOUNT, SdMount, BOOK_NAME_MAX};
use crate::sleep::{to_sleep, to_sleep_tips};
use crate::storage::{NvsStorage, SleepStorage};
use crate::widgets::list_widget::ListWidget;

const PAGES_VEC_MAX:usize = epd2in9_txt::PAGES_VEC_MAX;
const LOG_VEC_MAX:usize = epd2in9_txt::LOG_VEC_MAX;
const ONE_PAGE_CONTENT_LEN:usize = epd2in9_txt::ONE_PAGE_CONTENT_LEN;


/// Physical display dimensions (epd4in2)
const DISPLAY_WIDTH: u32 = 400;
const DISPLAY_HEIGHT: u32 = 300;
const FONT_SIZE: u32 = 16;
const PROGRESS_AREA_HEIGHT: u32 = 20;

const SLEEP_IMG_W: u32 = DISPLAY_HEIGHT; // 300 portrait width
const SLEEP_IMG_H: u32 = DISPLAY_WIDTH;  // 400 portrait height
const SLEEP_BUF_SIZE: usize = (SLEEP_IMG_W * SLEEP_IMG_H / 8) as usize; // 15000

static mut SLEEP_IMAGE_DATA: Option<AllocVec<u8>> = None;

/// Allocate sleep image buffer on heap (call when entering read_page).
fn alloc_sleep_image() {
    let mut buf = AllocVec::with_capacity(SLEEP_BUF_SIZE);
    buf.resize(SLEEP_BUF_SIZE, 0);
    unsafe { SLEEP_IMAGE_DATA = Some(buf); }
}

/// Free sleep image buffer (call when exiting read_page).
fn free_sleep_image() {
    unsafe { SLEEP_IMAGE_DATA = None; }
}

fn load_sleep_image(root: &mut ActualDirectory<'_>) {
    let Some(pixels) =(unsafe { SLEEP_IMAGE_DATA.as_mut() } )else { return };
    pixels.fill(0);

    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(_) => {
            println!("images dir not found, skip sleep image");
            return;
        }
    };
    let file = SdMount::open_file_by_name(&mut images_dir, "sleep.bmp", embedded_sdmmc::Mode::ReadOnly);
    let mut file = match file {
        Ok(f) => f,
        Err(_) => {
            println!("sleep.bmp not found");
            return;
        }
    };
    let len = file.length() as usize;
    if len == 0 || len > 30000 {
        println!("sleep.bmp invalid size: {}", len);
        file.close();
        return;
    }
    let mut raw = AllocVec::with_capacity(len);
    let mut buf = [0u8; 512];
    loop {
        match file.read(&mut buf) {
            Ok(n) if n > 0 => raw.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    file.close();
    // raw (temp AllocVec for BMP data) freed at end of this function

    if raw.len() < 54 || raw[0] != b'B' || raw[1] != b'M' {
        println!("sleep.bmp: invalid header");
        return;
    }
    let pixel_offset = u32::from_le_bytes([raw[10], raw[11], raw[12], raw[13]]) as usize;
    let bmp_w = u32::from_le_bytes([raw[18], raw[19], raw[20], raw[21]]);
    let height_raw = i32::from_le_bytes([raw[22], raw[23], raw[24], raw[25]]);
    let bpp = u16::from_le_bytes([raw[28], raw[29]]) as u32;
    let top_down = height_raw < 0;
    let bmp_h = height_raw.unsigned_abs();
    println!("sleep.bmp: {}x{} bpp={}", bmp_w, bmp_h, bpp);

    if bmp_w == 0 || bmp_h == 0 || (bpp != 1 && bpp != 24 && bpp != 32) {
        println!("sleep.bmp: unsupported format");
        return;
    }
    let row_bytes = match bpp {
        1 => (bmp_w + 7) / 8,
        24 => bmp_w * 3,
        32 => bmp_w * 4,
        _ => return,
    };
    let row_stride = ((row_bytes + 3) & !3u32) as usize;

    for dy in 0..SLEEP_IMG_H {
        let src_y = dy * bmp_h / SLEEP_IMG_H;
        let src_y = if top_down { src_y } else { bmp_h - 1 - src_y };
        let row_start = pixel_offset + src_y as usize * row_stride;
        for dx in 0..SLEEP_IMG_W {
            let sx = dx * bmp_w / SLEEP_IMG_W;
            let is_black = match bpp {
                1 => {
                    let byte_idx = row_start + sx as usize / 8;
                    let bit_idx = 7 - (sx % 8);
                    byte_idx < raw.len() && (raw[byte_idx] >> bit_idx) & 1 == 0
                }
                24 => {
                    let px = row_start + sx as usize * 3;
                    if px + 2 < raw.len() {
                        let (b, g, r) = (raw[px] as u32, raw[px+1] as u32, raw[px+2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else { false }
                }
                32 => {
                    let px = row_start + sx as usize * 4;
                    if px + 2 < raw.len() {
                        let (b, g, r) = (raw[px] as u32, raw[px+1] as u32, raw[px+2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else { false }
                }
                _ => false,
            };
            if is_black {
                let idx = (dy * SLEEP_IMG_W + dx) as usize;
                pixels[idx / 8] |= 1 << (7 - (idx % 8));
            }
        }
    }
    println!("sleep image loaded, {} bytes heap", SLEEP_BUF_SIZE);
}

fn sleep_renderer(display: &mut crate::display::EpdDisplay) {
    display.clear_buffer(Color::White);

    let drawn = unsafe {
        SLEEP_IMAGE_DATA.as_ref().map_or(false, |pixels| {
            for y in 0..SLEEP_IMG_H {
                for x in 0..SLEEP_IMG_W {
                    let idx = (y * SLEEP_IMG_W + x) as usize;
                    if (pixels[idx / 8] >> (7 - idx % 8)) & 1 != 0 {
                        use embedded_graphics::Pixel;
                        let _ = Pixel(Point::new(x as i32, y as i32), Black).draw(display);
                    }
                }
            }
            true
        })
    };

    if !drawn {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let mut font = font.with_ignore_unknown_chars(true);
        let center = Point::new(
            display.bounding_box().size.width as i32 / 2,
            display.bounding_box().size.height as i32 / 2,
        );
        let _ = font.render_aligned(
            "睡眠中",
            center,
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
    }
}


/// Accelerating step size for page jump long press.
/// 0-4 ticks: 1, 5-9: 5, 10-19: 10, 20-34: 50, 35+: 100
fn accel_step(tick: u32) -> u32 {
    if tick < 3 { 1 }
    else if tick < 5 { 5 }
    else if tick < 8 { 10 }
    else if tick < 10 { 50 }
    else if tick < 15 { 100 }
    else if tick < 20 { 200 }
    else { 400 }
}

const MENU_ITEMS: &[&str] = &["返回书单", "收藏书签", "打开书签", "删除书签", "跳转页码", "旋转屏幕", "重建索引", "睡眠", "取消"];

enum MenuState {
    Closed,
    Popup { menu_index: u32 },
    JumpInput { input_num: u32 },
    BookmarkList { bm_index: u32, deleting: bool },
}

pub struct ReadPage{
    running:bool,
    reading:bool,
    need_render:bool,
    change_page:bool,

    force_indexing:bool,
    indexing:bool,
    indexing_process:f32,

    choose_index:u32,
    open_file_name:String<BOOK_NAME_MAX>,
    menus:Option<Vec<String<BOOK_NAME_MAX>,40>>,
    book_pages:Option<BookPages>,
    log_vec:Option<Vec<u32,LOG_VEC_MAX>>,
    page_index:u32,
    page_content:String<ONE_PAGE_CONTENT_LEN>,
    menu_state:MenuState,
    save_bookmark_flag:bool,
    delete_bookmark_flag:bool,
    need_load_preview:bool,
    bookmark_preview:String<ONE_PAGE_CONTENT_LEN>,
    /// 0=Rotate90, 1=Rotate270 (upside-down portrait, same page indexing)
    flipped:bool,
    jump_accel:u32,
    book_progress:Vec<String<16>,40>,
}

impl ReadPage{
    fn current_rotation(&self) -> DisplayRotation {
        if self.flipped { DisplayRotation::Rotate270 } else { DisplayRotation::Rotate90 }
    }

    fn visual_width(&self) -> u32 { DISPLAY_HEIGHT } // 300, always portrait
    fn visual_height(&self) -> u32 { DISPLAY_WIDTH } // 400, always portrait

    fn page_lines(&self) -> u32 {
        (self.visual_height() - PROGRESS_AREA_HEIGHT) / FONT_SIZE - 1
    }

    async fn back(&mut self){
        self.running = false;
    }
    async fn get_page_vec<>(&mut self, books_dir:&mut ActualDirectory<'_>)  {
        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();

        let file_name = format!("{}.txt", book_name);
        let mut need_index = false;
        let mut file_len = 0;
        let mut book_pages = None;

        // Resolve book's short name via LFN lookup
        let book_short_name = match SdMount::find_entry_by_name(books_dir, &file_name) {
            Some(entry) => entry.name,
            None => {
                println!("Book not found: {}", file_name);
                return;
            }
        };

        {
            let mut my_file = books_dir.open_file_in_dir(book_short_name.clone(), embedded_sdmmc::Mode::ReadOnly).unwrap();
            file_len = my_file.length();
            my_file.close();
        }

        println!("file len:{}", file_len);
        {
            let mut my_file_index = SdMount::open_idx_file(books_dir, &book_short_name, embedded_sdmmc::Mode::ReadOnly);
            if let Ok(mut mfi) = my_file_index {
                println!("idx len:{}", mfi.length());
                if (mfi.length() == 0) {
                    need_index = true;
                } else {
                    println!("entry read pages");
                    //读索引
                    book_pages = Some(BookPages::new(mfi.length()));

                    if let Some(ref mut b) = book_pages {

                        if b.total_page == 0 {
                            need_index = true;
                        }else if b.get_end_page_position(&mut mfi)  != file_len{
                            need_index = true;
                        }

                        println!("book_pages:{:?}",*b);
                    }
                }
                mfi.close();
            } else {
                need_index = true;
            }
        }
        if need_index || self.force_indexing {


            self.indexing = true;
            let self_ptr = Self::mut_to_ptr(self);
            let short_name_clone = book_short_name.clone();
            let dw = self.visual_width();
            let dl = self.page_lines();
            book_pages = TxtReader::generate_pages(books_dir, book_name.as_str(), &short_name_clone, dw, dl, |process|  {
                return Box::pin(async  move {

                    let mut_ref:&mut Self =  Self::mut_by_ptr(Some(self_ptr)).unwrap();
                    mut_ref.indexing_process = process;
                    mut_ref.need_render = true;
                    mut_ref.render().await;
                    Timer::after_millis(500).await;
                });
            }).await;

            self.need_render = true;
            self.force_indexing = false;
            self.indexing = false;

        }

        self.book_pages = book_pages;

    }
    async fn get_log_vec(&mut self,books_dir:&mut ActualDirectory<'_>) {
        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
        let file_name = format!("{}.txt", book_name);

        // Resolve book's short name
        let book_short_name = match SdMount::find_entry_by_name(books_dir, &file_name) {
            Some(entry) => entry.name,
            None => return,
        };
        {
            //读日志
            let mut my_file =  SdMount::open_log_file(books_dir, &book_short_name, embedded_sdmmc::Mode::ReadOnly);
            if let Ok(mut f) = my_file {
                self.log_vec = Some(TxtReader::read_log(&mut f));
                if let Some(ref lv) = self.log_vec{
                    if lv.len() > 0 {
                        self.page_index = lv[0];
                    }
                }
                f.close();
            } else {
                // No log file yet — initialize empty so bookmark save works
                self.log_vec = Some(Vec::new());
            }
        }

    }
    async fn get_page_content(&mut self,books_dir:&mut ActualDirectory<'_>){

        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
        let begin_secs = Instant::now().as_secs();
        println!("begin_time:{}", begin_secs);

        let file_name = format!("{}.txt", book_name);

        // Resolve book's short name
        let book_short_name = match SdMount::find_entry_by_name(books_dir, &file_name) {
            Some(entry) => entry.name,
            None => return,
        };

        if let Some( bp) = self.book_pages.as_mut() {
            let mut begin =  0;
            let mut end =  0;
            {
                let mut index_file = SdMount::open_idx_file(books_dir, &book_short_name, embedded_sdmmc::Mode::ReadOnly);
                if let Ok(mut index_file) = index_file {
                    (begin, end) = bp.get_page_content_position(&mut index_file);
                }
            }
            {
                let mut my_file = books_dir.open_file_in_dir(book_short_name.clone(), embedded_sdmmc::Mode::ReadOnly);
                if let Ok(mut my_file) = my_file {
                    self.page_content = TxtReader::get_page_content(&mut my_file, begin, end, self.visual_width());
                    my_file.close();
                }
            }
            {
                let logfile = SdMount::open_log_file(books_dir, &book_short_name, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);
                if let Ok(mut f) = logfile {
                    if self.log_vec.is_none() {
                        self.log_vec = Some(Vec::new());
                    }
                   epd2in9_txt::TxtReader::save_log(&mut f,self.log_vec.as_mut().unwrap(), self.page_index as u32, false);
                    f.close();
                }else{
                    println!("log error:{:#?}",logfile.unwrap_err());
                }
            }
        }

    }

    fn load_book_progress(&mut self, books_dir: &mut ActualDirectory<'_>) {
        self.book_progress.clear();
        if let Some(ref menus) = self.menus {
            for book_name in menus.iter() {
                let file_name = alloc::format!("{}.txt", book_name);
                let short_name = match SdMount::find_entry_by_name(books_dir, &file_name) {
                    Some(entry) => entry.name,
                    None => {
                        let _ = self.book_progress.push(String::new());
                        continue;
                    }
                };
                let current_page: u32 = {
                    let log_result = SdMount::open_log_file(books_dir, &short_name, embedded_sdmmc::Mode::ReadOnly);
                    if let Ok(mut f) = log_result {
                        let log = TxtReader::read_log(&mut f);
                        f.close();
                        if !log.is_empty() { log[0] } else { 0 }
                    } else { 0 }
                };
                let total_page: u32 = {
                    let idx_result = SdMount::open_idx_file(books_dir, &short_name, embedded_sdmmc::Mode::ReadOnly);
                    if let Ok(mut f) = idx_result {
                        let len = f.length();
                        f.close();
                        len / 4
                    } else { 0 }
                };
                let mut s: String<16> = String::new();
                if total_page > 0 {
                    use core::fmt::Write;
                    let _ = write!(s, "{}%", current_page * 100 / total_page);
                }
                let _ = self.book_progress.push(s);
            }
        }
    }

    async fn do_change_page(&mut self,page_index:u32){
        if self.book_pages.is_none() { return; }

        if page_index >= self.book_pages.as_ref().unwrap().total_page {
            self.page_index = self.book_pages.as_ref().unwrap().total_page;
        }else{
            self.page_index = page_index;
        }
        self.change_page = true;
        self.need_render = true;

    }

    fn render_menu_overlay(&self, display: &mut crate::display::EpdDisplay) {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let mut font = font.with_ignore_unknown_chars(true);
        let vw = self.visual_width();
        let vh = self.visual_height();
        let menu_width: u32 = 180;
        let menu_item_height: u32 = 24;
        let menu_padding: u32 = 8;

        match self.menu_state {
            MenuState::Popup { menu_index } => {
                let page_info_height: u32 = 18;
                let menu_height = MENU_ITEMS.len() as u32 * menu_item_height + page_info_height + menu_padding * 2;
                let menu_x = ((vw - menu_width) / 2) as i32;
                let menu_y = ((vh - menu_height) / 2) as i32;

                let rect = Rectangle::new(
                    Point::new(menu_x, menu_y),
                    Size::new(menu_width, menu_height),
                );
                let style = PrimitiveStyleBuilder::new()
                    .fill_color(White)
                    .stroke_color(Black)
                    .stroke_alignment(StrokeAlignment::Outside)
                    .stroke_width(2)
                    .build();
                rect.into_styled(style).draw(display).ok();

                for (i, label) in MENU_ITEMS.iter().enumerate() {
                    let item_y = menu_y + menu_padding as i32 + (i as u32 * menu_item_height) as i32;
                    let is_selected = i as u32 == menu_index;

                    if is_selected {
                        let highlight = Rectangle::new(
                            Point::new(menu_x + 4, item_y),
                            Size::new(menu_width - 8, menu_item_height),
                        );
                        highlight.into_styled(
                            PrimitiveStyleBuilder::new().fill_color(Black).build()
                        ).draw(display).ok();
                    }

                    let prefix = if is_selected { "> " } else { "  " };
                    let text_color = if is_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                    font.render_aligned(
                        format_args!("{}{}", prefix, label),
                        Point::new(menu_x + menu_padding as i32, item_y + menu_item_height as i32 / 2),
                        VerticalPosition::Center,
                        HorizontalAlignment::Left,
                        text_color,
                        display,
                    ).ok();
                }

                // Show page number inside menu, at bottom
                if let Some(ref bp) = self.book_pages {
                    let total = bp.total_page;
                    if total > 0 {
                        let current = if self.page_index > total { total } else { self.page_index };
                        let page_text_y = menu_y + menu_height as i32 - menu_padding as i32;
                        font.render_aligned(
                            format_args!("{}/{}", current, total),
                            Point::new(menu_x + menu_width as i32 / 2, page_text_y),
                            VerticalPosition::Bottom,
                            HorizontalAlignment::Center,
                            FontColor::Transparent(Black),
                            display,
                        ).ok();
                    }
                }
            }
            MenuState::JumpInput { input_num } => {
                let jump_height: u32 = 90;
                let jump_x = ((vw - menu_width) / 2) as i32;
                let jump_y = ((vh - jump_height) / 2) as i32;
                let center_x = (vw / 2) as i32;

                let rect = Rectangle::new(
                    Point::new(jump_x, jump_y),
                    Size::new(menu_width, jump_height),
                );
                let style = PrimitiveStyleBuilder::new()
                    .fill_color(White)
                    .stroke_color(Black)
                    .stroke_alignment(StrokeAlignment::Outside)
                    .stroke_width(2)
                    .build();
                rect.into_styled(style).draw(display).ok();

                font.render_aligned(
                    "跳转页码",
                    Point::new(center_x, jump_y + 22),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                ).ok();

                let total = self.book_pages.as_ref().map(|b| b.total_page).unwrap_or(0);
                font.render_aligned(
                    format_args!("{} / {}", input_num, total),
                    Point::new(center_x, jump_y + 48),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                ).ok();

                font.render_aligned(
                    "1+ 2- 3确认 长按取消",
                    Point::new(center_x, jump_y + 72),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                ).ok();
            }
            MenuState::BookmarkList { bm_index, deleting } => {
                let bookmarks: Vec<u32, LOG_VEC_MAX> = self.log_vec.as_ref()
                    .map(|lv| lv.iter().skip(1).copied().collect())
                    .unwrap_or_default();

                let bm_count = bookmarks.len() as u32;
                // Max 4 bookmark items + cancel
                let max_visible = 4u32;
                let visible_bm = if bm_count > max_visible { max_visible } else { bm_count };
                let total_items = if visible_bm > 0 { visible_bm + 1 } else { 1 };
                let list_height = total_items * menu_item_height + menu_padding * 2;
                let list_x = ((vw - menu_width) / 2) as i32;
                let list_y = 20;

                // List border
                let rect = Rectangle::new(
                    Point::new(list_x, list_y),
                    Size::new(menu_width, list_height),
                );
                let style = PrimitiveStyleBuilder::new()
                    .fill_color(White)
                    .stroke_color(Black)
                    .stroke_alignment(StrokeAlignment::Outside)
                    .stroke_width(2)
                    .build();
                rect.into_styled(style).draw(display).ok();

                // Title
                let title = if deleting { "删除书签" } else { "书签列表" };
                font.render_aligned(
                    title,
                    Point::new(list_x + menu_width as i32 / 2, list_y + 10),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                ).ok();

                if bm_count == 0 {
                    font.render_aligned(
                        "暂无书签",
                        Point::new(list_x + menu_width as i32 / 2, list_y + menu_padding as i32 + menu_item_height as i32 + menu_item_height as i32 / 2),
                        VerticalPosition::Center,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    ).ok();
                } else {
                    // Scroll offset
                    let scroll_offset = if bm_index >= max_visible { bm_index - max_visible + 1 } else { 0 };
                    for vi in 0..visible_bm {
                        let bi = vi + scroll_offset;
                        if bi >= bm_count { break; }
                        let page = bookmarks[bi as usize];
                        let item_y = list_y + menu_padding as i32 + ((vi + 1) as u32 * menu_item_height) as i32;
                        let is_selected = bi == bm_index;

                        if is_selected {
                            let highlight = Rectangle::new(
                                Point::new(list_x + 4, item_y),
                                Size::new(menu_width - 8, menu_item_height),
                            );
                            highlight.into_styled(
                                PrimitiveStyleBuilder::new().fill_color(Black).build()
                            ).draw(display).ok();
                        }

                        let text_color = if is_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                        let prefix = if is_selected { "> " } else { "  " };
                        let delete_mark = if deleting && is_selected { " ×" } else { "" };
                        font.render_aligned(
                            format_args!("{}第{}页{}", prefix, page, delete_mark),
                            Point::new(list_x + menu_padding as i32, item_y + menu_item_height as i32 / 2),
                            VerticalPosition::Center,
                            HorizontalAlignment::Left,
                            text_color,
                            display,
                        ).ok();
                    }
                }

                // Cancel item
                let cancel_y = list_y + menu_padding as i32 + (total_items as i32) * menu_item_height as i32;
                let is_cancel_selected = bm_index >= bm_count;
                if is_cancel_selected {
                    let highlight = Rectangle::new(
                        Point::new(list_x + 4, cancel_y),
                        Size::new(menu_width - 8, menu_item_height),
                    );
                    highlight.into_styled(
                        PrimitiveStyleBuilder::new().fill_color(Black).build()
                    ).draw(display).ok();
                }
                let cancel_color = if is_cancel_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                let cancel_prefix = if is_cancel_selected { "> " } else { "  " };
                font.render_aligned(
                    format_args!("{}取消", cancel_prefix),
                    Point::new(list_x + menu_padding as i32, cancel_y + menu_item_height as i32 / 2),
                    VerticalPosition::Center,
                    HorizontalAlignment::Left,
                    cancel_color,
                    display,
                ).ok();

                // Preview area below the list
                if bm_count > 0 && bm_index < bm_count && !self.bookmark_preview.is_empty() {
                    let preview_y = list_y + list_height as i32 + 10;
                    let preview_height = vh as i32 - preview_y - 10;
                    if preview_height > 30 {
                        let preview_rect = Rectangle::new(
                            Point::new(list_x, preview_y),
                            Size::new(menu_width, preview_height as u32),
                        );
                        let preview_style = PrimitiveStyleBuilder::new()
                            .fill_color(White)
                            .stroke_color(Black)
                            .stroke_alignment(StrokeAlignment::Outside)
                            .stroke_width(1)
                            .build();
                        preview_rect.into_styled(preview_style).draw(display).ok();

                        use embedded_graphics::draw_target::DrawTargetExt;
                        let clipped = Rectangle::new(
                            Point::new(list_x + 4, preview_y + 4),
                            Size::new(menu_width - 8, (preview_height - 8) as u32),
                        );
                        let mut clipped_display = display.clipped(&clipped);
                        font.render_aligned(
                            self.bookmark_preview.as_str(),
                            Point::new(list_x + 6, preview_y + 14),
                            VerticalPosition::Top,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            &mut clipped_display,
                        ).ok();
                    }
                }
            }
            MenuState::Closed => {}
        }
    }

    /// Draw progress bar only at very bottom of display
    fn render_progress(&self, display: &mut crate::display::EpdDisplay) {
        if let Some(ref bp) = self.book_pages {
            let total = bp.total_page;
            if total == 0 { return; }
            let current = if self.page_index > total { total } else { self.page_index };
            let vw = self.visual_width();
            let vh = self.visual_height();
            let bar_height: u32 = 3;
            let margin: i32 = 2;
            let bottom = vh as i32;
            let bar_y = bottom - bar_height as i32 - margin;
            let bar_full_width = vw as i32 - margin * 2;

            // Background bar
            let bg = Rectangle::new(
                Point::new(margin, bar_y),
                Size::new(bar_full_width as u32, bar_height),
            );
            bg.into_styled(
                PrimitiveStyleBuilder::new().fill_color(White).stroke_color(Black).stroke_width(1).build()
            ).draw(display).ok();

            // Filled portion
            let filled_width = if total > 0 {
                ((current as u64 * bar_full_width as u64) / total as u64) as u32
            } else {
                0
            };
            if filled_width > 0 {
                let filled = Rectangle::new(
                    Point::new(margin, bar_y),
                    Size::new(filled_width, bar_height),
                );
                filled.into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build()).draw(display).ok();
            }
        }
    }


}
#[ram(rtc_fast)]
pub static mut PAGE_INDEX:Option<u32>   = None ;

impl Page for ReadPage{
    fn new() -> Self {
        

       let mut temp  = Self{
            running: false,
            reading: false,
            need_render: false,
            change_page:false,
            force_indexing: false,
            indexing: false,
            indexing_process: 0.0,
            choose_index: 0,
            open_file_name: Default::default(),
            menus: None,
            book_pages: None,
            log_vec: None,
            page_index:0,
            page_content: Default::default(),
            menu_state: MenuState::Closed,
            save_bookmark_flag: false,
            delete_bookmark_flag: false,
            need_load_preview: false,
            bookmark_preview: Default::default(),
            flipped: false,
            jump_accel: 0,
            book_progress: Vec::new(),
        };

        unsafe{
            if let Some(v) = PAGE_INDEX {
                temp.choose_index = v;
                temp.change_page = true;
                temp.reading = true;
                temp.book_pages = None;
            }
        } 
       
       temp
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;

            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(Color::White);
                let vw = self.visual_width();
                let vh = self.visual_height();
                let center = Point::new(vw as i32 / 2, vh as i32 / 2);

                if self.indexing {
                    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                    let mut font = font.with_ignore_unknown_chars(true);
                    let _ = font.render_aligned(
                        format_args!("正在创建索引，\n 已创建索引进度：{:.2}%",self.indexing_process),
                        center,
                        VerticalPosition::Center,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    );
                    println!("显示进度：{}",self.indexing_process);
                    crate::sleep::refresh_active_time().await;
                }else {
                    if !self.reading {
                        println!("in render");
                        if let Some(ref menus) = self.menus {
                            println!("in render menus");
                            let menus: Vec<&str, 20> = menus.iter().map(|v| { v.as_str() }).collect();
                            let mut list_widget = ListWidget::new(Point::new(0, 0)
                                                                  , Black
                                                                  , White
                                                                  , Size::new(vw, vh)
                                                                  , menus
                            );
                            list_widget.choose(self.choose_index as usize);
                            let _ = list_widget.draw(display);

                            // Draw book progress right-aligned
                            if self.book_progress.len() == self.menus.as_ref().unwrap().len() {
                                let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();
                                let mut font = font.with_ignore_unknown_chars(true);
                                let item_height: u32 = 20;
                                let scroll_width: u32 = 10;
                                let total_items = self.book_progress.len();
                                let content_h = total_items as u32 * item_height;
                                let scroll_offset: i32 = if content_h <= vh { 0 } else {
                                    let half = vh / 2;
                                    let max_off = content_h - vh;
                                    let cy = self.choose_index as u32 * item_height;
                                    if cy <= half { 0 }
                                    else if cy >= max_off + half { max_off as i32 }
                                    else { (cy - half) as i32 }
                                };
                                for bi in 0..total_items {
                                    if self.book_progress[bi].is_empty() { continue; }
                                    let item_y = bi as i32 * item_height as i32 - scroll_offset;
                                    if item_y < 0 || item_y + item_height as i32 > vh as i32 { continue; }
                                    font.render_aligned(
                                        self.book_progress[bi].as_str(),
                                        Point::new((vw - scroll_width - 5) as i32, item_y + 5),
                                        VerticalPosition::Top,
                                        HorizontalAlignment::Right,
                                        FontColor::Transparent(Black),
                                        display,
                                    ).ok();
                                }
                            }
                        }
                    } else if self.book_pages.is_some() {
                        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                        let mut font = font.with_ignore_unknown_chars(true);
                        //显示选择书本对应页的内容
                        display.clear_buffer(Color::White);
                        {
                            if self.page_index == self.book_pages.as_ref().unwrap().total_page {
                                let _ = font.render_aligned(
                                    "已是最后一页",
                                    center,
                                    VerticalPosition::Center,
                                    HorizontalAlignment::Center,
                                    FontColor::Transparent(Black),
                                    display,
                                );
                            } else {
                                let _ = font.render_aligned(
                                    self.page_content.as_str(),
                                    Point::new(0, 2),
                                    VerticalPosition::Top,
                                    HorizontalAlignment::Left,
                                    FontColor::Transparent(Black),
                                    display,
                                );
                            }
                        }
                        self.render_progress(display);
                    }
                }
                // Draw menu overlay on top of reading content
                if self.reading && !matches!(self.menu_state, MenuState::Closed) {
                    self.render_menu_overlay(display);
                }
            }


            RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;
        }
    }
    
    

    async fn run(&mut self, spawner: Spawner) {
        alloc_sleep_image();
        display::set_sleep_renderer(Some(sleep_renderer));
        if let Some(display) = display_mut() {
           display.set_rotation(self.current_rotation());
        }
        self.running = true;
        self.need_render = true;
        //*event::ENABLE_DOUBLE.lock().await = true;
        //读sd卡目录
        if let Some(ref mut sd) =  *SD_MOUNT.lock().await {

            let mut volume0 = sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0));
            match volume0 {
                Ok(mut v) => {
                    let root_result = v.open_root_dir();
                    match root_result {
                        Ok(mut root) => {
                            load_sleep_image(&mut root);
                            let books_dir_res = root.open_dir("books");
                            if let Ok(mut books_dir) = books_dir_res {
                                let books = SdMount::get_books(&mut books_dir).unwrap();
                                self.menus = Some(books);
                                self.load_book_progress(&mut books_dir);

                                loop {
                                    if !self.running { break; }
                                    if self.menus.as_ref().unwrap().len() > 0 {
                                        if let None = self.book_pages {
                                            self.get_page_vec(&mut books_dir).await;
                                            self.get_log_vec(&mut books_dir).await;
                                        }
                                        if self.change_page {
                                            println!("change_page : {}", self.page_index);
                                            self.change_page = false;
                                            self.book_pages.as_mut().unwrap().set_current_page(self.page_index);
                                            self.get_page_content(&mut books_dir).await;
                                        }
                                    }

                                    // Handle bookmark save (needs books_dir)
                                    if self.save_bookmark_flag {
                                        self.save_bookmark_flag = false;
                                        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                        let file_name = format!("{}.txt", book_name);
                                        if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                            let short_name = entry.name;
                                            let logfile = SdMount::open_log_file(&mut books_dir, &short_name, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);
                                            if let Ok(mut f) = logfile {
                                                if self.log_vec.is_none() {
                                                    self.log_vec = Some(Vec::new());
                                                }
                                                if let Some(ref mut lv) = self.log_vec {
                                                    TxtReader::save_log(&mut f, lv, self.page_index, true);
                                                }
                                                f.close();
                                            }
                                        }
                                    }

                                    // Handle bookmark delete (needs books_dir)
                                    if self.delete_bookmark_flag {
                                        self.delete_bookmark_flag = false;
                                        if let Some(ref mut lv) = self.log_vec {
                                            let bm_idx = match self.menu_state {
                                                MenuState::BookmarkList { bm_index, .. } => bm_index,
                                                _ => 0,
                                            } as usize;
                                            // bookmarks are at lv[1..], so delete at bm_idx + 1
                                            let del_idx = bm_idx + 1;
                                            if del_idx < lv.len() {
                                                lv.remove(del_idx);
                                                // Write updated log
                                                let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                                let file_name = format!("{}.txt", book_name);
                                                if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                                    let short_name = entry.name;
                                                    let logfile = SdMount::open_log_file(&mut books_dir, &short_name, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);
                                                    if let Ok(mut f) = logfile {
                                                        TxtReader::save_log_raw(&mut f, lv);
                                                        f.close();
                                                    }
                                                }
                                                // Adjust bm_index if needed
                                                let new_bm_count = if lv.len() > 1 { lv.len() - 1 } else { 0 };
                                                if let MenuState::BookmarkList { ref mut bm_index, .. } = self.menu_state {
                                                    if *bm_index as usize >= new_bm_count && *bm_index > 0 {
                                                        *bm_index -= 1;
                                                    }
                                                }
                                            }
                                        }
                                        self.bookmark_preview.clear();
                                        self.need_render = true;
                                    }

                                    // Handle bookmark preview loading (needs books_dir)
                                    if self.need_load_preview {
                                        self.need_load_preview = false;
                                        self.bookmark_preview.clear();
                                        let bm_page = match self.menu_state {
                                            MenuState::BookmarkList { bm_index, .. } => {
                                                self.log_vec.as_ref()
                                                    .and_then(|lv| lv.iter().skip(1).nth(bm_index as usize).copied())
                                            },
                                            _ => None,
                                        };
                                        if let Some(page) = bm_page {
                                            if self.book_pages.is_some() {
                                                let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                                let file_name = format!("{}.txt", book_name);
                                                if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                                    let short_name = entry.name;
                                                    // Read page position from index file (own block to release borrow)
                                                    let page_pos = {
                                                        let mut idx_file = SdMount::open_idx_file(&mut books_dir, &short_name, embedded_sdmmc::Mode::ReadOnly);
                                                        if let Ok(mut idx_file) = idx_file {
                                                            let begin_pos = if page == 0 { 0u32 } else {
                                                                idx_file.seek_from_start((page - 1) * 4);
                                                                let mut buf = [0u8; 4];
                                                                let _ = idx_file.read(&mut buf);
                                                                ((buf[0] as u32) << 24) | ((buf[1] as u32) << 16) | ((buf[2] as u32) << 8) | buf[3] as u32
                                                            };
                                                            idx_file.seek_from_start(page * 4);
                                                            let mut buf = [0u8; 4];
                                                            let end_pos = if idx_file.read(&mut buf).unwrap_or(0) == 4 {
                                                                ((buf[0] as u32) << 24) | ((buf[1] as u32) << 16) | ((buf[2] as u32) << 8) | buf[3] as u32
                                                            } else { 0 };
                                                            idx_file.close();
                                                            Some((begin_pos, end_pos))
                                                        } else { None }
                                                    };

                                                    if let Some((begin_pos, end_pos)) = page_pos {
                                                        if end_pos > begin_pos {
                                                            let mut my_file = books_dir.open_file_in_dir(short_name, embedded_sdmmc::Mode::ReadOnly);
                                                            if let Ok(mut my_file) = my_file {
                                                                self.bookmark_preview = TxtReader::get_page_content(&mut my_file, begin_pos, end_pos, self.visual_width());
                                                                my_file.close();
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        self.need_render = true;
                                    }

                                    if !matches!(self.menu_state, MenuState::Closed) {
                                        display::reset_render_times();
                                    }
                                    self.render().await;

                                    // Skip sleep timer when menu is open
                                    if matches!(self.menu_state, MenuState::Closed) {
                                        let sleep_storage = crate::storage::SleepStorage::read().unwrap_or_default();
                                        let read_sleep_seconds = if sleep_storage.read_sleep_seconds > 0 {
                                            sleep_storage.read_sleep_seconds
                                        } else {
                                            120
                                        };
                                        to_sleep_tips(Duration::from_secs(0), Duration::from_secs(read_sleep_seconds),true).await;
                                    }

                                    Timer::after_millis(50).await;
                                }
                            }
                        },
                        Err(er) => {
                            println!("open volume:{:?}", er);
                            display::show_error("打开主目录失败",true).await;
                        },
                    }
                },
                Err(e) => {
                    println!("open volume:{:?}", e);
                    display::show_error("读取分区失败",true).await;
                }
            }
        }
        //*event::ENABLE_DOUBLE.lock().await = false;
        free_sleep_image();
        display::set_sleep_renderer(None);
        if let Some(display) = display_mut() {
            display.set_rotation(DisplayRotation::Rotate0);
        }
    }

    async fn bind_event(&mut self) {
        event::clear().await;

        // Key3 long: open/close menu (in reading mode) or exit ReadPage
        event::on_target(EventType::KeyLongEnd(3),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::JumpInput { .. } => {
                        // 取消跳转，返回菜单
                        mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                        mut_ref.need_render = true;
                    }
                    MenuState::BookmarkList { .. } => {
                        // 取消书签列表，返回菜单
                        mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                        mut_ref.need_render = true;
                    }
                    _ => {
                        if mut_ref.reading {
                            // 长按退出阅读，回到书单
                            mut_ref.reading = false;
                            unsafe { PAGE_INDEX = None; }
                            mut_ref.menu_state = MenuState::Closed;
                            mut_ref.need_render = true;
                        } else {
                            mut_ref.back().await;
                        }
                    }
                }
            });
        }).await;

        // Key3 short: open/close menu / select menu item / confirm jump / toggle reading mode
        event::on_target(EventType::KeyShort(3),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { menu_index } => {
                        match menu_index {
                            0 => {
                                // 返回书单
                                mut_ref.reading = false;
                                unsafe { PAGE_INDEX = None; }
                            }
                            1 => {
                                // 收藏书签
                                mut_ref.save_bookmark_flag = true;
                            }
                            2 => {
                                // 打开书签 — 正常模式
                                mut_ref.menu_state = MenuState::BookmarkList { bm_index: 0, deleting: false };
                                mut_ref.need_load_preview = true;
                                mut_ref.need_render = true;
                                return;
                            }
                            3 => {
                                // 删除书签 — 删除模式
                                mut_ref.menu_state = MenuState::BookmarkList { bm_index: 0, deleting: true };
                                mut_ref.bookmark_preview.clear();
                                mut_ref.need_render = true;
                                return;
                            }
                            4 => {
                                // 跳转页码 — 进入数字输入
                                mut_ref.menu_state = MenuState::JumpInput { input_num: mut_ref.page_index };
                                mut_ref.jump_accel = 0;
                                mut_ref.need_render = true;
                                return;
                            }
                            5 => {
                                // 旋转屏幕 — flip upside down (Rotate90 ↔ Rotate270)
                                mut_ref.flipped = !mut_ref.flipped;
                                if let Some(display) = display_mut() {
                                    display.set_rotation(mut_ref.current_rotation());
                                }
                            }
                            6 => {
                                // 重建索引
                                mut_ref.force_indexing = true;
                                mut_ref.book_pages = None;
                            }
                            7 => {
                                // 睡眠
                                crate::sleep::refresh_active_time().await;
                                crate::sleep::to_sleep_tips(Duration::from_secs(0), Duration::from_secs(0), true).await;
                                return;
                            }
                            _ => {}
                        }
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::JumpInput { input_num } => {
                        // 确认跳转
                        mut_ref.do_change_page(input_num).await;
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::BookmarkList { bm_index, deleting } => {
                        let bm_count = mut_ref.log_vec.as_ref().map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 }).unwrap_or(0) as u32;
                        if bm_index >= bm_count {
                            // 取消 — 返回菜单
                            mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                            mut_ref.bookmark_preview.clear();
                            mut_ref.need_render = true;
                        } else if deleting {
                            // 删除选中书签
                            mut_ref.delete_bookmark_flag = true;
                            mut_ref.need_render = true;
                        } else if let Some(ref lv) = mut_ref.log_vec {
                            let bookmarks: Vec<u32, LOG_VEC_MAX> = lv.iter().skip(1).copied().collect();
                            if (bm_index as usize) < bookmarks.len() {
                                mut_ref.do_change_page(bookmarks[bm_index as usize]).await;
                            }
                            mut_ref.menu_state = MenuState::Closed;
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            // 短按打开菜单
                            mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                            mut_ref.need_render = true;
                        } else {
                            mut_ref.reading = true;
                            mut_ref.change_page = true;
                            mut_ref.page_index = 0;
                            mut_ref.book_pages = None;
                            unsafe { PAGE_INDEX = Some(mut_ref.choose_index); }
                            mut_ref.need_render = true;
                        }
                    }
                }
            });
        }).await;

        // Key1 long hold: continuous scroll down (book list or menu) / accelerating page jump
        event::on_target(EventType::KeyLongIng(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        if *menu_index < (MENU_ITEMS.len() - 1) as u32 {
                            *menu_index += 1;
                        } else {
                            *menu_index = 0;
                        }
                        mut_ref.need_render = true;
                        Timer::after_millis(200).await;
                    }
                    MenuState::JumpInput { ref mut input_num } => {
                        let max_page = mut_ref.book_pages.as_ref().map(|b| b.total_page).unwrap_or(9999);
                        let step = accel_step(mut_ref.jump_accel);
                        mut_ref.jump_accel += 1;
                        if *input_num + step <= max_page {
                            *input_num += step;
                        } else {
                            *input_num = max_page;
                        }
                        mut_ref.need_render = true;
                        Timer::after_millis(75).await;
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        let bm_count = mut_ref.log_vec.as_ref().map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 }).unwrap_or(0) as u32;
                        if *bm_index < bm_count {
                            *bm_index += 1;
                            if !deleting { mut_ref.need_load_preview = true; }
                            mut_ref.need_render = true;
                            Timer::after_millis(200).await;
                        }
                    }
                    MenuState::Closed => {
                        if !mut_ref.reading {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 && mut_ref.choose_index < (max - 1) as u32 {
                                mut_ref.choose_index += 1;
                                display::reset_render_times();
                                mut_ref.need_render = true;
                                Timer::after_millis(200).await;
                            }
                        }
                    }
                    _ => {}
                }
            });
        }).await;

        // Key2 long hold: continuous scroll up (book list or menu) / accelerating page jump
        event::on_target(EventType::KeyLongIng(2),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        if *menu_index > 0 {
                            *menu_index -= 1;
                        } else {
                            *menu_index = (MENU_ITEMS.len() - 1) as u32;
                        }
                        mut_ref.need_render = true;
                        Timer::after_millis(200).await;
                    }
                    MenuState::JumpInput { ref mut input_num } => {
                        let step = accel_step(mut_ref.jump_accel);
                        mut_ref.jump_accel += 1;
                        if *input_num >= step {
                            *input_num -= step;
                        } else {
                            *input_num = 0;
                        }
                        mut_ref.need_render = true;
                        Timer::after_millis(75).await;
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        if *bm_index > 0 {
                            *bm_index -= 1;
                            if !deleting { mut_ref.need_load_preview = true; }
                            mut_ref.need_render = true;
                            Timer::after_millis(200).await;
                        }
                    }
                    MenuState::Closed => {
                        if !mut_ref.reading {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 && mut_ref.choose_index > 0 {
                                mut_ref.choose_index -= 1;
                                display::reset_render_times();
                                mut_ref.need_render = true;
                                Timer::after_millis(200).await;
                            }
                        }
                    }
                    _ => {}
                }
            });
        }).await;

        // Key1 short: menu down / jump +1 / next page / next book
        event::on_target(EventType::KeyShort(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        if *menu_index < (MENU_ITEMS.len() - 1) as u32 {
                            *menu_index += 1;
                        } else {
                            *menu_index = 0;
                        }
                        mut_ref.need_render = true;
                    }
                    MenuState::JumpInput { ref mut input_num } => {
                        let max_page = mut_ref.book_pages.as_ref().map(|b| b.total_page).unwrap_or(9999);
                        if *input_num < max_page {
                            *input_num += 1;
                            mut_ref.jump_accel = 0;
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        let bm_count = mut_ref.log_vec.as_ref().map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 }).unwrap_or(0) as u32;
                        if *bm_index < bm_count {
                            *bm_index += 1;
                            if !deleting { mut_ref.need_load_preview = true; }
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            mut_ref.do_change_page(mut_ref.page_index + 1).await;
                        } else {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 {
                                if mut_ref.choose_index < (max - 1) as u32 {
                                    mut_ref.choose_index += 1;
                                } else {
                                    mut_ref.choose_index = 0;
                                }
                            }
                            mut_ref.need_render = true;
                        }
                    }
                }
            });
        }).await;

        // Key2 short: menu up / jump -1 / prev page / prev book
        event::on_target(EventType::KeyShort(2),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        if *menu_index > 0 {
                            *menu_index -= 1;
                        } else {
                            *menu_index = (MENU_ITEMS.len() - 1) as u32;
                        }
                        mut_ref.need_render = true;
                    }
                    MenuState::JumpInput { ref mut input_num } => {
                        if *input_num > 0 {
                            *input_num -= 1;
                            mut_ref.jump_accel = 0;
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        if *bm_index > 0 {
                            *bm_index -= 1;
                            if !deleting { mut_ref.need_load_preview = true; }
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            if mut_ref.page_index > 0 {
                                mut_ref.do_change_page(mut_ref.page_index - 1).await;
                            }
                        } else {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 {
                                if mut_ref.choose_index > 0 {
                                    mut_ref.choose_index -= 1;
                                } else {
                                    mut_ref.choose_index = (max - 1) as u32;
                                }
                            }
                            mut_ref.need_render = true;
                        }
                    }
                }
            });
        }).await;
    }
}