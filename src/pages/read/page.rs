use alloc::boxed::Box;
use alloc::format;
use core::str::FromStr;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::prelude::Primitive;
use epd_waveshare::color::{Black, Color, White};
use epd_waveshare::graphics::{Display, DisplayRotation};
use esp_hal::ram;
use esp_println::println;
use heapless::{String, Vec};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::{display, epd2in9_txt, event};
use crate::epd2in9_txt::TxtReader;
use crate::event::EventType;
use crate::pages::Page;
use crate::sd_mount::{ActualDirectory, ActualFile, SD_MOUNT, SdMount, BOOK_NAME_MAX};
use crate::sleep::to_sleep_tips;
use crate::storage::NvsStorage;
use crate::widgets::list_widget::ListWidget;

const LOG_VEC_MAX: usize = epd2in9_txt::LOG_VEC_MAX;
const ONE_PAGE_CONTENT_LEN: usize = epd2in9_txt::ONE_PAGE_CONTENT_LEN;

/// Accelerating step size for page jump long press.
fn accel_step(tick: u32) -> u32 {
    if tick < 3 { 1 }
    else if tick < 5 { 5 }
    else if tick < 8 { 10 }
    else if tick < 10 { 50 }
    else if tick < 15 { 100 }
    else if tick < 20 { 200 }
    else { 400 }
}

/// 判断文件名是否为 .ttf（大小写不敏感）。
fn is_ttf(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 4 && b[b.len() - 4..].eq_ignore_ascii_case(b".ttf")
}

// 菜单项：0=返回书单 1=收藏书签 2=打开书签 3=删除书签 4=跳转进度
//         5=排版 6=字体 7=旋转屏幕 8=睡眠 else=取消
const MENU_ITEMS: &[&str] = &[
    "返回书单", "收藏书签", "打开书签", "删除书签", "跳转进度",
    "排版", "字体", "旋转屏幕", "睡眠", "取消",
];

enum MenuState {
    Closed,
    Popup { menu_index: u32 },
    JumpInput { input_pct: u8 },
    /// 排版：px/gap 为进入时的快照（供“长按取消”还原），实时值在 self.ttf_px / self.line_gap。
    Layout { px: u8, gap: u8 },
    FontList { font_index: u32 },
    BookmarkList { bm_index: u32, deleting: bool },
}

pub struct ReadPage {
    running: bool,
    reading: bool,
    need_render: bool,
    choose_index: u32,
    menus: Option<Vec<String<BOOK_NAME_MAX>, 40>>,
    log_vec: Option<Vec<u32, LOG_VEC_MAX>>,
    menu_state: MenuState,
    save_bookmark_flag: bool,
    delete_bookmark_flag: bool,
    /// 翻页/跳转后写 .log[0]（持久化阅读位置到 SD，防掉电丢失）。
    need_save_position: bool,
    need_load_preview: bool,
    bookmark_preview: String<ONE_PAGE_CONTENT_LEN>,
    /// 屏幕旋转状态（0..3，相对默认依次向右 +90°）。
    rotate: u8,
    exit_selected: bool,
    jump_accel: u32,
    book_progress: Vec<String<16>, 40>,
    /// run() 里 books_dir 的裸指针（绕过 Page::render 无参 trait 限制）。
    /// 仅在 run() 作用域内有效（设置于打开 books_dir 后、退出前置空）。
    books_dir_ptr: *mut ActualDirectory<'static>,
    /// 字体文件句柄的裸指针（整个阅读会话复用同一句柄，换字体时重开）。
    font_file_ptr: *mut ActualFile<'static>,
    /// TTF 动态分页状态：按字节偏移即时分页。
    ttf_offset: u32,
    ttf_end: u32,
    ttf_file_len: u32,
    /// 全书页索引：每页的起始字节偏移（进书时一次性建好）。
    ttf_page_starts: Vec<u32, 1000>,
    /// 当前在 ttf_page_starts 中的索引。
    ttf_page_idx: u32,
    ttf_px: f32,
    /// 行间距（像素，叠加在 fi.line_height 之上）。
    line_gap: i32,
    /// TTF 渲染工作区（堆分配，进阅读创建、退出释放）。
    ttf_ws: Option<&'static mut crate::ttf_sd::TtfWs>,
    /// 续读标志：get_page_vec 据此保留预置的 ttf_offset（不清零），
    /// get_log_vec 据此跳过 .log[0] 还原（rtc_fast 的 TTF_RESUME_OFFSET 是权威）。
    ttf_resume: bool,
    /// 当前字体文件名。
    font_name: String<32>,
    /// SD 卡上可用的 .ttf 列表（进 run 时扫描一次）。
    font_list: Vec<String<32>, 10>,
    /// 换字体标志：run 循环据此重开字体 + 清字形/字体表缓存。
    need_reload_font: bool,
}

impl ReadPage {
    async fn back(&mut self) {
        self.running = false;
    }

    /// 把当前 ttf_px/line_gap/rotate/font_name 打包写进 ReadingSettings（flash）。
    fn save_reading_settings(&self) {
        let mut s = crate::storage::ReadingSettings {
            ttf_px: ((self.ttf_px as i32).max(12).min(40)) as u8,
            line_gap: self.line_gap.max(0).min(255) as u8,
            rotate: self.rotate,
            font_name: [0u8; 32],
        };
        let bytes = self.font_name.as_bytes();
        let n = bytes.len().min(s.font_name.len());
        s.font_name[..n].copy_from_slice(&bytes[..n]);
        let _ = s.write();
    }

    /// 跳到指定字节偏移（跳转进度/书签）。清历史、保存续读偏移、触发渲染。
    fn jump_to_offset(&mut self, off: u64) {
        let off = off.min(self.ttf_file_len as u64) as u32;
        self.ttf_offset = off;
        self.ttf_end = off;
        // 在页索引中找最近的页
        if !self.ttf_page_starts.is_empty() {
            let mut idx = 0u32;
            for i in 0..self.ttf_page_starts.len() {
                if self.ttf_page_starts[i] <= off { idx = i as u32; } else { break; }
            }
            self.ttf_page_idx = idx;
            self.ttf_offset = self.ttf_page_starts[idx as usize];
            self.ttf_end = self.ttf_offset;
        }
        unsafe {
            *core::ptr::addr_of_mut!(TTF_RESUME_OFFSET) = off;
        }
        self.need_save_position = true;
        self.need_render = true;
    }

    /// 打开选中的书 .txt：取文件长度、初始化 TTF 分页状态。
    /// ttf_resume=true（重启分模式续读）：保留预置偏移（夹到 file_len），不清 ttf_resume。
    /// 否则（新开书）：偏移归零，由 get_log_vec 从 .log[0] 还原上次位置。
    async fn get_page_vec(&mut self, books_dir: &mut ActualDirectory<'_>) {
        let book_name = match self.menus.as_ref().and_then(|m| m.get(self.choose_index as usize)) {
            Some(n) => n.clone(),
            None => return,
        };
        let file_name = format!("{}.txt", book_name);
        let file_len = match SdMount::find_entry_by_name(books_dir, &file_name) {
            Some(entry) => {
                let short = entry.name;
                match books_dir.open_file_in_dir(short, embedded_sdmmc::Mode::ReadOnly) {
                    Ok(mut f) => {
                        let l = f.length();
                        f.close();
                        l
                    }
                    Err(_) => return,
                }
            }
            None => {
                println!("Book not found: {}", file_name);
                return;
            }
        };
        println!("file len:{}", file_len);
        self.ttf_file_len = file_len;
        if self.ttf_resume {
            // 续读：保留预置偏移（夹到文件长度内），不清 ttf_resume（get_log_vec 据此跳过 log[0] 覆盖）
            if self.ttf_offset > file_len {
                self.ttf_offset = 0;
            }
            self.ttf_end = self.ttf_offset;
        } else {
            self.ttf_offset = 0;
            self.ttf_end = 0;
        }
        self.ttf_page_starts.clear();
        self.ttf_page_idx = 0;
    }

    /// 读 .log 进 log_vec。ttf_resume：跳过 .log[0] 还原（rtc_fast 权威），清 ttf_resume。
    /// 否则：用 .log[0] 还原 ttf_offset（夹到 file_len）。
    async fn get_log_vec(&mut self, books_dir: &mut ActualDirectory<'_>) {
        let book_name = match self.menus.as_ref().and_then(|m| m.get(self.choose_index as usize)) {
            Some(n) => n.clone(),
            None => return,
        };
        let file_name = format!("{}.txt", book_name);
        let short_name = match SdMount::find_entry_by_name(books_dir, &file_name) {
            Some(e) => e.name,
            None => return,
        };
        let log_file = SdMount::open_log_file(books_dir, &short_name, embedded_sdmmc::Mode::ReadOnly);
        if let Ok(mut f) = log_file {
            self.log_vec = Some(TxtReader::read_log(&mut f));
            f.close();
        } else {
            self.log_vec = Some(Vec::new());
        }
        if self.ttf_resume {
            // 续读：rtc_fast 的 TTF_RESUME_OFFSET 是权威的，不被 .log[0] 覆盖
            self.ttf_resume = false;
        } else if let Some(ref lv) = self.log_vec {
            if let Some(&off) = lv.first() {
                self.ttf_offset = off.min(self.ttf_file_len);
                self.ttf_end = self.ttf_offset;
            }
        }
    }

    /// 书单每本书的阅读进度（字节偏移 ÷ .txt 长度 → "X%"）。
    fn load_book_progress(&mut self, books_dir: &mut ActualDirectory<'_>) {
        self.book_progress.clear();
        if let Some(ref menus) = self.menus {
            for book_name in menus.iter() {
                let file_name = alloc::format!("{}.txt", book_name);
                let (offset, total) = match SdMount::find_entry_by_name(books_dir, &file_name) {
                    Some(entry) => {
                        let short = entry.name;
                        let total = match books_dir.open_file_in_dir(short.clone(), embedded_sdmmc::Mode::ReadOnly) {
                            Ok(mut f) => {
                                let l = f.length();
                                f.close();
                                l
                            }
                            Err(_) => 0,
                        };
                        let offset = match SdMount::open_log_file(books_dir, &short, embedded_sdmmc::Mode::ReadOnly) {
                            Ok(mut f) => {
                                let log = TxtReader::read_log(&mut f);
                                f.close();
                                log.first().copied().unwrap_or(0)
                            }
                            Err(_) => 0,
                        };
                        (offset, total)
                    }
                    None => {
                        let _ = self.book_progress.push(String::new());
                        continue;
                    }
                };
                let mut s: String<16> = String::new();
                if total > 0 {
                    use core::fmt::Write;
                    let _ = write!(s, "{}%", offset * 100 / total);
                }
                let _ = self.book_progress.push(s);
            }
        }
    }

    /// TTF 翻页：用页索引直接索引加减，O(1)。
    async fn do_change_page(&mut self, next: bool) {
        let total = self.ttf_page_starts.len() as u32;
        if total == 0 { return; }
        if next {
            if self.ttf_page_idx + 1 < total {
                self.ttf_page_idx += 1;
            }
        } else if self.ttf_page_idx > 0 {
            self.ttf_page_idx -= 1;
        }
        self.ttf_offset = self.ttf_page_starts[self.ttf_page_idx as usize];
        self.ttf_end = self.ttf_offset;
        unsafe {
            *core::ptr::addr_of_mut!(TTF_RESUME_OFFSET) = self.ttf_offset;
        }
        self.need_save_position = true;
        self.need_render = true;
    }

    /// 预加载下一页字形：渲染完当前页后提前光栅化进缓存（不画），下次翻页直接 blit。
    async fn preload_next_page(&mut self) {
        let ttf_end = self.ttf_end;
        let ttf_file_len = self.ttf_file_len;
        let ttf_px = self.ttf_px;
        let line_gap = self.line_gap;
        let bd_ptr = self.books_dir_ptr;
        let font_ptr = self.font_file_ptr;
        let vh = super::visual_height();
        let book_name = match self
            .menus
            .as_ref()
            .and_then(|m| m.get(self.choose_index as usize))
            .cloned()
        {
            Some(n) => n,
            None => return,
        };
        if ttf_file_len == 0 || ttf_end >= ttf_file_len || bd_ptr.is_null() {
            return;
        }
        if let Some(ws) = self.ttf_ws.as_mut() {
            let bd: &mut ActualDirectory<'_> = unsafe { &mut *bd_ptr };
            let file_name = format!("{}.txt", book_name);
            let short_name = SdMount::find_entry_by_name(bd, &file_name).map(|e| e.name);
            // ① 读下一页字节
            let mut n = 0usize;
            if let Some(sn) = short_name {
                if let Ok(mut bf) = bd.open_file_in_dir(sn, embedded_sdmmc::Mode::ReadOnly) {
                    n = crate::ttf_sd::read_book_chunk(&mut bf, ttf_end, ws);
                    bf.close();
                }
            }
            // ② 预加载字形（只缓存不画），复用会话级字体句柄
            if n > 0 && !font_ptr.is_null() {
                let ff: &mut ActualFile = unsafe { &mut *font_ptr };
                if let Some(fi) = crate::ttf_sd::open_font(ff, ws) {
                    let text_area_h = vh as i32 - super::PROGRESS_AREA_HEIGHT as i32 - 2;
                    let base_h = (fi.line_height(ttf_px) + line_gap).max(1);
                    let max_lines = ((text_area_h / base_h) as u32).max(1);
                    let line_h = text_area_h / max_lines as i32;
                    crate::ttf_sd::preload_glyphs(
                        ff, &fi, ws, n, ttf_px,
                        super::text_width() as i32, line_h, max_lines,
                    )
                    .await;
                }
            }
        }
    }

    fn render_menu_overlay(&self, display: &mut crate::display::EpdDisplay) {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let font = font.with_ignore_unknown_chars(true);
        let vw = super::visual_width();
        let vh = super::visual_height();
        let menu_width: u32 = if vw < 200 { vw - 16 } else { 180 };
        let menu_item_height: u32 = 24;
        let menu_padding: u32 = 8;

        let box_style = PrimitiveStyleBuilder::new()
            .fill_color(White)
            .stroke_color(Black)
            .stroke_alignment(StrokeAlignment::Outside)
            .stroke_width(2)
            .build();

        match self.menu_state {
            MenuState::Popup { menu_index } => {
                let page_info_height: u32 = 18;
                let menu_height = MENU_ITEMS.len() as u32 * menu_item_height + page_info_height + menu_padding * 2;
                let menu_x = ((vw - menu_width) / 2) as i32;
                let menu_y = ((vh - menu_height) / 2) as i32;

                Rectangle::new(Point::new(menu_x, menu_y), Size::new(menu_width, menu_height))
                    .into_styled(box_style)
                    .draw(display)
                    .ok();

                for (i, label) in MENU_ITEMS.iter().enumerate() {
                    let item_y = menu_y + menu_padding as i32 + (i as u32 * menu_item_height) as i32;
                    let is_selected = i as u32 == menu_index;
                    if is_selected {
                        Rectangle::new(
                            Point::new(menu_x + 4, item_y),
                            Size::new(menu_width - 8, menu_item_height),
                        )
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
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
                    )
                    .ok();
                }

                if self.ttf_file_len > 0 {
                    let pct = self.ttf_offset.min(self.ttf_file_len) * 100 / self.ttf_file_len;
                    let page_text_y = menu_y + menu_height as i32 - menu_padding as i32;
                    font.render_aligned(
                        format_args!("{}%", pct),
                        Point::new(menu_x + menu_width as i32 / 2, page_text_y),
                        VerticalPosition::Bottom,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    )
                    .ok();
                }
            }
            MenuState::JumpInput { input_pct } => {
                let jump_height: u32 = 100;
                let jump_x = ((vw - menu_width) / 2) as i32;
                let jump_y = ((vh - jump_height) / 2) as i32;
                let center_x = (vw / 2) as i32;

                Rectangle::new(Point::new(jump_x, jump_y), Size::new(menu_width, jump_height))
                    .into_styled(box_style)
                    .draw(display)
                    .ok();

                font.render_aligned(
                    "跳转进度",
                    Point::new(center_x, jump_y + 22),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();

                font.render_aligned(
                    format_args!("{}%", input_pct),
                    Point::new(center_x, jump_y + 50),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();

                // 进度条预览
                let bar_y = jump_y + 70;
                let bar_w = menu_width as i32 - 24;
                Rectangle::new(Point::new(jump_x + 12, bar_y), Size::new(bar_w as u32, 4))
                    .into_styled(
                        PrimitiveStyleBuilder::new().fill_color(White).stroke_color(Black).stroke_width(1).build(),
                    )
                    .draw(display)
                    .ok();
                let fw = (bar_w as u32 * input_pct as u32 / 100) as i32;
                if fw > 0 {
                    Rectangle::new(Point::new(jump_x + 12, bar_y), Size::new(fw as u32, 4))
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
                }
            }
            MenuState::Layout { .. } => {
                let h: u32 = 120;
                let x = ((vw - menu_width) / 2) as i32;
                let y = ((vh - h) / 2) as i32;
                let cx = x + menu_width as i32 / 2;

                Rectangle::new(Point::new(x, y), Size::new(menu_width, h))
                    .into_styled(box_style)
                    .draw(display)
                    .ok();

                font.render_aligned(
                    "排版设置",
                    Point::new(cx, y + 18),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
                font.render_aligned(
                    format_args!("字号: {}", self.ttf_px as i32),
                    Point::new(cx, y + 48),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
                font.render_aligned(
                    format_args!("行距: {}", self.line_gap),
                    Point::new(cx, y + 74),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
                font.render_aligned(
                    "1/2字号 长按行距 3保存",
                    Point::new(cx, y + h as i32 - 14),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
            }
            MenuState::FontList { font_index } => {
                let count = self.font_list.len() as u32;
                let h = vh - 20;
                let x = ((vw - menu_width) / 2) as i32;
                let y: i32 = 10;
                let cx = x + menu_width as i32 / 2;

                Rectangle::new(Point::new(x, y), Size::new(menu_width, h))
                    .into_styled(box_style)
                    .draw(display)
                    .ok();

                font.render_aligned(
                    "选择字体",
                    Point::new(cx, y + 14),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
                let sep_y = y + 28;
                Rectangle::new(Point::new(x + 4, sep_y), Size::new(menu_width - 8, 1))
                    .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                    .draw(display)
                    .ok();

                let item_h: i32 = 22;
                let list_top = sep_y + 4;
                let list_bottom = y + h as i32 - 24; // 预留提示行
                let list_h = (list_bottom - list_top).max(0);
                let visible = if item_h > 0 { (list_h / item_h) as u32 } else { 1 }.max(1);
                let scroll_offset: u32 = if count <= visible {
                    0
                } else if font_index >= count {
                    count - visible
                } else if font_index >= visible {
                    font_index - visible + 1
                } else {
                    0
                };

                if count == 0 {
                    font.render_aligned(
                        "未找到字体",
                        Point::new(cx, (list_top + list_bottom) / 2),
                        VerticalPosition::Center,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    )
                    .ok();
                }

                for vi in 0..visible {
                    let fi = scroll_offset + vi;
                    if fi >= count {
                        break;
                    }
                    let name = &self.font_list[fi as usize];
                    let iy = list_top + vi as i32 * item_h;
                    let is_sel = fi == font_index;
                    let is_cur = name.as_str() == self.font_name.as_str();
                    if is_sel {
                        Rectangle::new(
                            Point::new(x + 3, iy),
                            Size::new(menu_width - 6, item_h as u32),
                        )
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
                    }
                    let color = if is_sel { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                    let prefix = if is_sel { "> " } else { "  " };
                    let mark = if is_cur { " ✓" } else { "" };
                    font.render_aligned(
                        format_args!("{}{}{}", prefix, name, mark),
                        Point::new(x + menu_padding as i32, iy + item_h / 2),
                        VerticalPosition::Center,
                        HorizontalAlignment::Left,
                        color,
                        display,
                    )
                    .ok();
                }

                font.render_aligned(
                    "1/2选择 3确认 长按取消",
                    Point::new(cx, y + h as i32 - 12),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
            }
            MenuState::BookmarkList { bm_index, deleting } => {
                let bookmarks: Vec<u32, LOG_VEC_MAX> = self
                    .log_vec
                    .as_ref()
                    .map(|lv| lv.iter().skip(1).copied().collect())
                    .unwrap_or_default();
                let bm_count = bookmarks.len() as u32;

                let margin: i32 = 8;
                let gap: i32 = 6;
                let total_h = vh as i32 - margin * 2;
                let list_h = total_h / 3;
                let preview_h = total_h - list_h - gap;
                let list_x = ((vw as i32 - menu_width as i32) / 2) as i32;
                let list_y = margin;
                let list_right = list_x + menu_width as i32;
                let preview_x: i32 = 0;
                let preview_y = margin + list_h + gap;

                Rectangle::new(Point::new(list_x, list_y), Size::new(menu_width, list_h as u32))
                    .into_styled(
                        PrimitiveStyleBuilder::new()
                            .fill_color(White)
                            .stroke_color(Black)
                            .stroke_alignment(StrokeAlignment::Inside)
                            .stroke_width(2)
                            .build(),
                    )
                    .draw(display)
                    .ok();

                let title_h: i32 = 18;
                let title = if deleting { "删除书签" } else { "书签列表" };
                font.render_aligned(
                    title,
                    Point::new(list_x + menu_width as i32 / 2, list_y + 4 + title_h / 2),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();
                let sep_y = list_y + 4 + title_h;
                Rectangle::new(Point::new(list_x + 4, sep_y), Size::new(menu_width - 8, 1))
                    .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                    .draw(display)
                    .ok();

                // 取消行（固定底部）
                let cancel_h: i32 = menu_item_height as i32;
                let cancel_y = list_y + list_h - 4 - cancel_h;
                let is_cancel_selected = bm_index >= bm_count;
                if is_cancel_selected {
                    Rectangle::new(Point::new(list_x + 3, cancel_y), Size::new(menu_width - 6, menu_item_height))
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
                }
                let cancel_color = if is_cancel_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                let cancel_prefix = if is_cancel_selected { "> " } else { "  " };
                font.render_aligned(
                    format_args!("{}取消", cancel_prefix),
                    Point::new(list_x + menu_padding as i32, cancel_y + cancel_h / 2),
                    VerticalPosition::Center,
                    HorizontalAlignment::Left,
                    cancel_color,
                    display,
                )
                .ok();

                // 书签滚动区
                let scroll_top = sep_y + 2;
                let scroll_bottom = cancel_y - 2;
                let scroll_h = (scroll_bottom - scroll_top).max(0);
                let item_h_i = menu_item_height as i32;
                let sb_w: i32 = 3;
                let sb_x = list_right - sb_w - 4;
                let text_left = list_x + menu_padding as i32;
                let text_right = sb_x - 4;
                let max_visible = if item_h_i > 0 { (scroll_h / item_h_i) as u32 } else { 1 }.max(1);

                let scroll_offset: u32 = if bm_count <= max_visible {
                    0
                } else if is_cancel_selected {
                    bm_count - max_visible
                } else if bm_index > bm_count - max_visible {
                    bm_count - max_visible
                } else if bm_index >= max_visible {
                    bm_index - max_visible + 1
                } else {
                    0
                };

                for vi in 0..max_visible {
                    let bi = scroll_offset + vi;
                    if bi >= bm_count {
                        break;
                    }
                    let off = bookmarks[bi as usize];
                    let item_y = scroll_top + (vi as i32) * item_h_i;
                    let is_selected = bi == bm_index;
                    if is_selected {
                        Rectangle::new(
                            Point::new(list_x + 3, item_y),
                            Size::new((text_right - list_x - 3) as u32, menu_item_height),
                        )
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
                    }
                    let text_color = if is_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                    let prefix = if is_selected { "> " } else { "  " };
                    let delete_mark = if deleting && is_selected { " ×" } else { "" };
                    let pct = if self.ttf_file_len > 0 { off * 100 / self.ttf_file_len } else { 0 };
                    font.render_aligned(
                        format_args!("{}{}%{}", prefix, pct, delete_mark),
                        Point::new(text_left, item_y + item_h_i / 2),
                        VerticalPosition::Center,
                        HorizontalAlignment::Left,
                        text_color,
                        display,
                    )
                    .ok();
                }

                if bm_count > max_visible && scroll_h > 0 {
                    Rectangle::new(Point::new(sb_x, scroll_top), Size::new(sb_w as u32, scroll_h as u32))
                        .into_styled(
                            PrimitiveStyleBuilder::new().fill_color(White).stroke_color(Black).stroke_width(1).build(),
                        )
                        .draw(display)
                        .ok();
                    let thumb_h = ((scroll_h as u32 * max_visible) / bm_count).max(6).min(scroll_h as u32);
                    let thumb_y = scroll_top + ((scroll_h as u32 * scroll_offset) / bm_count) as i32;
                    Rectangle::new(Point::new(sb_x, thumb_y), Size::new(sb_w as u32, thumb_h))
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
                }

                if bm_count == 0 {
                    font.render_aligned(
                        "暂无书签",
                        Point::new(list_x + menu_width as i32 / 2, (scroll_top + scroll_bottom) / 2),
                        VerticalPosition::Center,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    )
                    .ok();
                }

                // 预览区（全屏宽，2/3 高）
                Rectangle::new(Point::new(preview_x, preview_y), Size::new(vw, preview_h as u32))
                    .into_styled(
                        PrimitiveStyleBuilder::new()
                            .fill_color(White)
                            .stroke_color(Black)
                            .stroke_alignment(StrokeAlignment::Inside)
                            .stroke_width(1)
                            .build(),
                    )
                    .draw(display)
                    .ok();

                if bm_count > 0
                    && bm_index < bm_count
                    && !self.bookmark_preview.is_empty()
                    && preview_h > 12
                {
                    use embedded_graphics::draw_target::DrawTargetExt;
                    let clip = Rectangle::new(
                        Point::new(preview_x + 4, preview_y + 4),
                        Size::new(vw - 8, (preview_h - 8) as u32),
                    );
                    let mut clipped_display = display.clipped(&clip);
                    font.render_aligned(
                        self.bookmark_preview.as_str(),
                        Point::new(preview_x + 6, preview_y + 14),
                        VerticalPosition::Top,
                        HorizontalAlignment::Left,
                        FontColor::Transparent(Black),
                        &mut clipped_display,
                    )
                    .ok();
                }
            }
            MenuState::Closed => {}
        }
    }

    /// 底部字节偏移百分比进度条。
    fn render_progress(&self, display: &mut crate::display::EpdDisplay) {
        let total = self.ttf_file_len;
        if total == 0 {
            return;
        }
        let current = self.ttf_offset.min(total);
        let vw = super::visual_width();
        let vh = super::visual_height();
        let bar_height: u32 = 3;
        let margin: i32 = 2;
        let bar_y = vh as i32 - bar_height as i32 - margin;
        let bar_full_width = vw as i32 - margin * 2;

        Rectangle::new(Point::new(margin, bar_y), Size::new(bar_full_width as u32, bar_height))
            .into_styled(
                PrimitiveStyleBuilder::new().fill_color(White).stroke_color(Black).stroke_width(1).build(),
            )
            .draw(display)
            .ok();

        let filled_width = ((current as u64 * bar_full_width as u64) / total as u64) as u32;
        if filled_width > 0 {
            Rectangle::new(Point::new(margin, bar_y), Size::new(filled_width, bar_height))
                .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                .draw(display)
                .ok();
        }
    }
}

#[ram(unstable(rtc_fast))]
pub(crate) static mut PAGE_INDEX: Option<u32> = None;

/// 本次启动是否进阅读模式（true=跳过 WiFi，独占堆做 TTF）。
/// rtc_fast：跨 reboot_sleep 深睡保留。
#[ram(unstable(rtc_fast))]
pub(crate) static mut READING_MODE: bool = false;

/// 跨复位续读的字节偏移（配合 PAGE_INDEX 标记的书）。rtc_fast：跨深睡保留。
#[ram(unstable(rtc_fast))]
static mut TTF_RESUME_OFFSET: u32 = 0;

impl Page for ReadPage {
    fn new() -> Self {
        // 读取排版设置（clamp 防止 flash 未初始化时的乱码值）
        let settings = crate::storage::ReadingSettings::read().unwrap_or_default();
        let ttf_px = settings.ttf_px.max(12).min(40) as f32;
        let line_gap = (settings.line_gap as i32).max(0).min(10);
        let rotate = settings.rotate % 4;
        let mut font_name: String<32> = String::new();
        let fname = settings.font_name_str();
        if is_ttf(fname) {
            let _ = font_name.push_str(fname);
        } else {
            let _ = font_name.push_str("font.ttf");
        }

        let mut temp = Self {
            running: false,
            reading: false,
            need_render: false,
            choose_index: 0,
            menus: None,
            log_vec: None,
            menu_state: MenuState::Closed,
            save_bookmark_flag: false,
            delete_bookmark_flag: false,
            need_save_position: false,
            need_load_preview: false,
            bookmark_preview: String::new(),
            rotate,
            exit_selected: false,
            jump_accel: 0,
            book_progress: Vec::new(),
            books_dir_ptr: core::ptr::null_mut(),
            font_file_ptr: core::ptr::null_mut(),
            ttf_offset: 0,
            ttf_end: 0,
            ttf_file_len: 0,
            ttf_page_starts: Vec::new(),
            ttf_page_idx: 0,
            ttf_px,
            line_gap,
            ttf_ws: None,
            ttf_resume: false,
            font_name,
            font_list: Vec::new(),
            need_reload_font: false,
        };

        unsafe {
            if let Some(v) = *core::ptr::addr_of!(PAGE_INDEX) {
                temp.choose_index = v;
                temp.reading = true;
                // 续读：恢复跨复位保存的字节偏移（重启分模式下从上次位置继续）
                temp.ttf_offset = *core::ptr::addr_of!(TTF_RESUME_OFFSET);
                temp.ttf_resume = true;
            }
        }

        temp
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;

            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(Color::White);
                let vw = super::visual_width();
                let vh = super::visual_height();
                let center = Point::new(vw as i32 / 2, vh as i32 / 2);

                if !self.reading {
                    // 书单列表
                    if let Some(ref menus) = self.menus {
                        let mut all_items: Vec<&str, 20> = Vec::new();
                        let _ = all_items.push("退出");
                        for item in menus.iter() {
                            if all_items.push(item.as_str()).is_err() {
                                break;
                            }
                        }
                        let widget_index = if self.exit_selected { 0usize } else { self.choose_index as usize + 1 };
                        let widget_index = widget_index.min(all_items.len().saturating_sub(1));

                        let mut list_widget = ListWidget::new(Point::new(0, 0), Black, White, Size::new(vw, vh), all_items);
                        list_widget.choose(widget_index);
                        let _ = list_widget.draw(display);

                        if self.book_progress.len() == menus.len() {
                            let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();
                            let font = font.with_ignore_unknown_chars(true);
                            let item_height: u32 = 20;
                            let scroll_width: u32 = 10;
                            let total_widget_items = self.book_progress.len() + 1;
                            let content_h = total_widget_items as u32 * item_height;
                            let scroll_offset: i32 = if content_h <= vh {
                                0
                            } else {
                                let half = vh / 2;
                                let max_off = content_h - vh;
                                let cy = widget_index as u32 * item_height;
                                if cy <= half {
                                    0
                                } else if cy >= max_off + half {
                                    max_off as i32
                                } else {
                                    (cy - half) as i32
                                }
                            };
                            for bi in 0..self.book_progress.len() {
                                if self.book_progress[bi].is_empty() {
                                    continue;
                                }
                                let item_y = (bi as i32 + 1) * item_height as i32 - scroll_offset;
                                if item_y < 0 || item_y + item_height as i32 > vh as i32 {
                                    continue;
                                }
                                font.render_aligned(
                                    self.book_progress[bi].as_str(),
                                    Point::new((vw - scroll_width - 5) as i32, item_y + 5),
                                    VerticalPosition::Top,
                                    HorizontalAlignment::Right,
                                    FontColor::Transparent(Black),
                                    display,
                                )
                                .ok();
                            }
                        }
                    }
                } else {
                    // 阅读态：TTF 即时分页渲染
                    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                    let font = font.with_ignore_unknown_chars(true);

                    let is_last = self.ttf_page_starts.len() > 0
                        && (self.ttf_page_idx as usize + 1) >= self.ttf_page_starts.len();
                    if is_last {
                        let _ = font.render_aligned(
                            "已是最后一页",
                            center,
                            VerticalPosition::Center,
                            HorizontalAlignment::Center,
                            FontColor::Transparent(Black),
                            display,
                        );
                    } else {
                        let ttf_offset = self.ttf_offset;
                        let ttf_px = self.ttf_px;
                        let line_gap = self.line_gap;
                        let bd_ptr = self.books_dir_ptr;
                        let font_ptr = self.font_file_ptr;
                        let book_name = self
                            .menus
                            .as_ref()
                            .and_then(|m| m.get(self.choose_index as usize))
                            .cloned();

                        // 渲染结果：Some(消费字节数) 成功；None 失败。
                        let mut result: Option<u32> = None;
                        if let (Some(ws), Some(book_name)) = (self.ttf_ws.as_mut(), book_name) {
                            if !bd_ptr.is_null() {
                                let bd: &mut ActualDirectory<'_> = unsafe { &mut *bd_ptr };
                                let file_name = format!("{}.txt", book_name);
                                let short_name =
                                    SdMount::find_entry_by_name(bd, &file_name).map(|e| e.name);
                                let mut n = 0usize;
                                if let Some(sn) = short_name {
                                    if let Ok(mut bf) =
                                        bd.open_file_in_dir(sn, embedded_sdmmc::Mode::ReadOnly)
                                    {
                                        n = crate::ttf_sd::read_book_chunk(&mut bf, ttf_offset, ws);
                                        bf.close();
                                    }
                                }
                                if n > 0 && !font_ptr.is_null() {
                                    let ff: &mut ActualFile = unsafe { &mut *font_ptr };
                                    if let Some(fi) = crate::ttf_sd::open_font(ff, ws) {
                                        let text_area_h = vh as i32
                                            - super::PROGRESS_AREA_HEIGHT as i32 - 2;
                                        let base_h = (fi.line_height(ttf_px) + line_gap).max(1);
                                        let max_lines = ((text_area_h / base_h) as u32).max(1);
                                        let line_h = text_area_h / max_lines as i32;
                                        let consumed = crate::ttf_sd::paginate_render(
                                            ff, &fi, ws, display, n, ttf_px,
                                            Point::new(super::text_left_margin(), 2),
                                            super::text_width() as i32, line_h, max_lines,
                                        );
                                        result = Some(consumed as u32);
                                    }
                                }
                            }
                        }

                        match result {
                            Some(consumed) => {
                                self.ttf_end = ttf_offset + consumed;
                            }
                            None => {
                                let msg = if self.ttf_ws.is_none() {
                                    "TTF 内存不足"
                                } else {
                                    "TTF 渲染失败"
                                };
                                let _ = font.render_aligned(
                                    msg,
                                    center,
                                    VerticalPosition::Center,
                                    HorizontalAlignment::Center,
                                    FontColor::Transparent(Black),
                                    display,
                                );
                            }
                        }
                    }
                    self.render_progress(display);
                }

                if self.reading && !matches!(self.menu_state, MenuState::Closed) {
                    self.render_menu_overlay(display);
                }
            }

            RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
        }
    }

    async fn run(&mut self, _spawner: Spawner) {
        display::set_sleep_renderer(Some(super::sleep_renderer));
        if let Some(display) = display_mut() {
            super::set_rotation_state(self.rotate);
            display.set_rotation(super::current_rotation());
        }

        // 阅读模式（重启分模式）：本启动未初始化 WiFi，堆几乎全空给 TTF。
        // 分配 TtfWs 工作区（大缓存）。重试 8 次等堆就绪。
        self.ttf_ws = None;
        let mut tries = 0u32;
        while self.ttf_ws.is_none() && tries < 8 {
            self.ttf_ws = crate::ttf_sd::alloc_ws();
            if self.ttf_ws.is_none() {
                tries += 1;
                Timer::after(Duration::from_millis(200)).await;
            }
        }
        if self.ttf_ws.is_none() {
            println!("[read ttf] alloc_ws 失败");
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
                            let books_dir_res = root.open_dir("books");
                            if let Ok(mut books_dir) = books_dir_res {
                                let books = match SdMount::get_books(&mut books_dir) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        println!("get_books error: {:?}", e);
                                        Vec::new()
                                    }
                                };
                                self.menus = Some(books);
                                self.load_book_progress(&mut books_dir);
                                self.books_dir_ptr =
                                    core::ptr::addr_of_mut!(books_dir) as *mut ActualDirectory<'static>;

                                // 扫描 .ttf 文件进 font_list（长文件名 + 短文件名回退）
                                {
                                    let mut fonts_found: Vec<String<32>, 10> = Vec::new();
                                    let mut storage = [0u8; 512];
                                    let mut lfn_buffer = embedded_sdmmc::LfnBuffer::new(&mut storage);
                                    let _ = books_dir.iterate_dir_lfn(&mut lfn_buffer, |dir, lfn| {
                                        let name_opt: Option<alloc::string::String> = if let Some(lfn_name) = lfn {
                                            if is_ttf(lfn_name) { Some(alloc::string::String::from(lfn_name)) } else { None }
                                        } else {
                                            // 短文件名回退：dir.name 是 ShortFileName
                                            if dir.name.extension().eq_ignore_ascii_case(b"TTF") {
                                                let base = dir.name.base_name();
                                                let mut s = alloc::string::String::from_utf8(alloc::vec::Vec::from(base)).unwrap_or_default();
                                                s.push_str(".TTF");
                                                Some(s)
                                            } else { None }
                                        };
                                        if let Some(name) = name_opt {
                                            if name.len() <= 32 {
                                                if let Ok(s) = String::<32>::from_str(&name) {
                                                    let _ = fonts_found.push(s);
                                                }
                                            }
                                        }
                                    });
                                    self.font_list = fonts_found;
                                }
                                // font_name 不在列表里则回退到第一个可用字体
                                if !self.font_list.is_empty()
                                    && !self.font_list.iter().any(|f| f.as_str() == self.font_name.as_str())
                                {
                                    self.font_name = self.font_list[0].clone();
                                }

                                // 打开字体句柄（整个会话复用，换字体时由 need_reload_font 重开）
                                let mut ttf_font_file: Option<ActualFile<'static>> = None;
                                let fname = self.font_name.as_str();
                                if let Ok(f) = SdMount::open_file_by_name(
                                    &mut books_dir,
                                    fname,
                                    embedded_sdmmc::Mode::ReadOnly,
                                ) {
                                    // 句柄实际只依赖 volume_mgr（整个阅读期有效），擦除生命周期为 'static。
                                    let f_static: ActualFile<'static> = unsafe { core::mem::transmute(f) };
                                    ttf_font_file = Some(f_static);
                                } else {
                                    println!("[read ttf] {} 打开失败", fname);
                                }
                                self.font_file_ptr = match ttf_font_file.as_mut() {
                                    Some(f) => f as *mut ActualFile<'static>,
                                    None => core::ptr::null_mut(),
                                };

                                // 一次性建立全书页索引（所有页的起始字节偏移）
                                if self.ttf_page_starts.is_empty()
                                    && self.ttf_file_len > 0
                                    && !self.font_file_ptr.is_null()
                                {
                                    let ff2: &mut ActualFile =
                                        unsafe { &mut *self.font_file_ptr };
                                    if let Some(ws) = self.ttf_ws.as_mut() {
                                        if let Some(fi) =
                                            crate::ttf_sd::open_font(ff2, ws)
                                        {
                                            let line_h =
                                                fi.line_height(self.ttf_px) + self.line_gap;
                                            let tah = super::visual_height() as i32
                                                - super::PROGRESS_AREA_HEIGHT as i32
                                                - 2;
                                            let ml = ((tah / line_h).max(1)) as u32;
                                            let alh = tah / ml as i32;
                                            let mw = super::text_width() as i32;
                                            let bn = self.menus.as_ref().and_then(|m| {
                                                m.get(self.choose_index as usize)
                                            }).cloned();
                                            if let Some(bn) = bn {
                                                let bd3: &mut ActualDirectory<'_> =
                                                    unsafe { &mut *self.books_dir_ptr };
                                                let fn3 = format!("{}.txt", bn);
                                                let sn3 = SdMount::find_entry_by_name(bd3, &fn3)
                                                    .map(|e| e.name);
                                                if let Some(sn3) = sn3 {
                                                    if let Ok(mut bf3) = bd3.open_file_in_dir(
                                                        sn3,
                                                        embedded_sdmmc::Mode::ReadOnly,
                                                    ) {
                                                        let _ = self.ttf_page_starts.push(0);
                                                        let mut off = 0u32;
                                                        loop {
                                                            let c = crate::ttf_sd::compute_page_consumed(
                                                                &mut bf3, ff2, &fi, ws, off,
                                                                self.ttf_px, mw, alh, ml,
                                                            );
                                                            if c == 0 { break; }
                                                            off += c;
                                                            if off >= self.ttf_file_len { break; }
                                                            if self.ttf_page_starts.push(off).is_err() { break; }
                                                        }
                                                        bf3.close();
                                                        // 根据续读偏移定位当前页索引
                                                        for i in 0..self.ttf_page_starts.len() {
                                                            if self.ttf_page_starts[i] <= self.ttf_offset {
                                                                self.ttf_page_idx = i as u32;
                                                            } else { break; }
                                                        }
                                                        println!("[read] 页索引: {} 页, 当前第 {} 页",
                                                            self.ttf_page_starts.len(),
                                                            self.ttf_page_idx + 1);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                loop {
                                    if !self.running {
                                        break;
                                    }
                                    if self.menus.as_ref().map(|m| m.len()).unwrap_or(0) > 0 {
                                        if self.ttf_file_len == 0 {
                                            self.get_page_vec(&mut books_dir).await;
                                            self.get_log_vec(&mut books_dir).await;
                                        }
                                    }

                                    // 收藏书签（当前位置字节偏移）
                                    if self.save_bookmark_flag {
                                        self.save_bookmark_flag = false;
                                        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                        let file_name = format!("{}.txt", book_name);
                                        if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                            let short_name = entry.name;
                                            let logfile = SdMount::open_log_file(
                                                &mut books_dir,
                                                &short_name,
                                                embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                                            );
                                            if let Ok(mut f) = logfile {
                                                if self.log_vec.is_none() {
                                                    self.log_vec = Some(Vec::new());
                                                }
                                                if let Some(ref mut lv) = self.log_vec {
                                                    TxtReader::save_log(&mut f, lv, self.ttf_offset, true);
                                                }
                                                f.close();
                                            }
                                        }
                                        self.need_render = true;
                                    }

                                    // 持久化阅读位置到 .log[0]
                                    if self.need_save_position {
                                        self.need_save_position = false;
                                        if self.ttf_file_len > 0 {
                                            let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                            let file_name = format!("{}.txt", book_name);
                                            if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                                let short_name = entry.name;
                                                let logfile = SdMount::open_log_file(
                                                    &mut books_dir,
                                                    &short_name,
                                                    embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                                                );
                                                if let Ok(mut f) = logfile {
                                                    if self.log_vec.is_none() {
                                                        self.log_vec = Some(Vec::new());
                                                    }
                                                    if let Some(ref mut lv) = self.log_vec {
                                                        TxtReader::save_log(&mut f, lv, self.ttf_offset, false);
                                                    }
                                                    f.close();
                                                }
                                            }
                                        }
                                    }

                                    // 换字体：重开句柄 + 清字形缓存 + 清字体表缓存（fi）
                                    if self.need_reload_font {
                                        self.need_reload_font = false;
                                        ttf_font_file.take();
                                        let fname = self.font_name.as_str();
                                        if let Ok(f) = SdMount::open_file_by_name(
                                            &mut books_dir,
                                            fname,
                                            embedded_sdmmc::Mode::ReadOnly,
                                        ) {
                                            let f_static: ActualFile<'static> =
                                                unsafe { core::mem::transmute(f) };
                                            ttf_font_file = Some(f_static);
                                            self.font_file_ptr = ttf_font_file.as_mut().unwrap() as *mut ActualFile<'static>;
                                            if let Some(ws) = self.ttf_ws.as_mut() {
                                                crate::ttf_sd::cache_clear(ws);
                                                ws.fi = None; // 强制重新解析新字体的表目录/度量
                                            }
                                            self.ttf_end = self.ttf_offset.min(self.ttf_file_len);
                                        } else {
                                            println!("[read ttf] 重开字体失败: {}", fname);
                                            self.font_file_ptr = core::ptr::null_mut();
                                        }
                                        self.need_render = true;
                                    }

                                    // 删除书签
                                    if self.delete_bookmark_flag {
                                        self.delete_bookmark_flag = false;
                                        if let Some(ref mut lv) = self.log_vec {
                                            let bm_idx = match self.menu_state {
                                                MenuState::BookmarkList { bm_index, .. } => bm_index,
                                                _ => 0,
                                            } as usize;
                                            let del_idx = bm_idx + 1;
                                            if del_idx < lv.len() {
                                                lv.remove(del_idx);
                                                let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                                let file_name = format!("{}.txt", book_name);
                                                if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                                    let short_name = entry.name;
                                                    let logfile = SdMount::open_log_file(
                                                        &mut books_dir,
                                                        &short_name,
                                                        embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                                                    );
                                                    if let Ok(mut f) = logfile {
                                                        TxtReader::save_log_raw(&mut f, lv);
                                                        f.close();
                                                    }
                                                }
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

                                    // 加载书签预览（读 .txt 在该书签偏移处的开头一段）
                                    if self.need_load_preview {
                                        self.need_load_preview = false;
                                        self.bookmark_preview.clear();
                                        let bm_off = match self.menu_state {
                                            MenuState::BookmarkList { bm_index, .. } => self
                                                .log_vec
                                                .as_ref()
                                                .and_then(|lv| lv.iter().skip(1).nth(bm_index as usize).copied()),
                                            _ => None,
                                        };
                                        if let Some(off) = bm_off {
                                            let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                            let file_name = format!("{}.txt", book_name);
                                            if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &file_name) {
                                                if let Ok(mut f) = books_dir.open_file_in_dir(
                                                    entry.name,
                                                    embedded_sdmmc::Mode::ReadOnly,
                                                ) {
                                                    let _ = f.seek_from_start(off);
                                                    let mut buf = [0u8; 512];
                                                    let mut got = 0usize;
                                                    while got < buf.len() {
                                                        match f.read(&mut buf[got..]) {
                                                            Ok(0) | Err(_) => break,
                                                            Ok(n) => got += n,
                                                        }
                                                    }
                                                    f.close();
                                                    if let Ok(s) = core::str::from_utf8(&buf[..got]) {
                                                        let _ = self.bookmark_preview.push_str(s);
                                                    }
                                                }
                                            }
                                        }
                                        self.need_render = true;
                                    }

                                    if !matches!(self.menu_state, MenuState::Closed) {
                                        display::reset_render_times();
                                    }
                                    let did_render = self.need_render;
                                    self.render().await;

                                    // 仅在实际渲染后（翻页）预加载一次下一页字形
                                    if did_render
                                        && matches!(self.menu_state, MenuState::Closed)
                                        && self.reading
                                    {
                                        self.preload_next_page().await;
                                    }

                                    if matches!(self.menu_state, MenuState::Closed) {
                                        let sleep_storage =
                                            crate::storage::SleepStorage::read().unwrap_or_default();
                                        let read_sleep_seconds = if sleep_storage.read_sleep_seconds > 0 {
                                            sleep_storage.read_sleep_seconds
                                        } else {
                                            120
                                        };
                                        to_sleep_tips(
                                            Duration::from_secs(0),
                                            Duration::from_secs(read_sleep_seconds),
                                            true,
                                        )
                                        .await;
                                    }

                                    Timer::after_millis(50).await;
                                }

                                // 退出：保存阅读位置到 .log[0]
                                if self.reading && self.ttf_file_len > 0 {
                                    let book_name = self
                                        .menus
                                        .as_ref()
                                        .and_then(|m| m.get(self.choose_index as usize))
                                        .cloned();
                                    if let Some(book_name) = book_name {
                                        let file_name = format!("{}.txt", book_name);
                                        if let Some(entry) =
                                            SdMount::find_entry_by_name(&mut books_dir, &file_name)
                                        {
                                            let short_name = entry.name;
                                            if let Ok(mut f) = SdMount::open_log_file(
                                                &mut books_dir,
                                                &short_name,
                                                embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                                            ) {
                                                if self.log_vec.is_none() {
                                                    self.log_vec = Some(Vec::new());
                                                }
                                                if let Some(ref mut lv) = self.log_vec {
                                                    TxtReader::save_log(&mut f, lv, self.ttf_offset, false);
                                                }
                                                f.close();
                                            }
                                        }
                                    }
                                }
                            } else {
                                println!("books dir not found");
                                self.menus = Some(Vec::new());
                                loop {
                                    if !self.running {
                                        break;
                                    }
                                    self.render().await;
                                    Timer::after_millis(50).await;
                                }
                            }
                        }
                        Err(er) => {
                            println!("open root:{:?}", er);
                            display::show_error("打开主目录失败", true).await;
                        }
                    }
                }
                Err(e) => {
                    println!("open volume:{:?}", e);
                    display::show_error("读取分区失败", true).await;
                }
            }
        }
        display::set_sleep_renderer(None);
        self.books_dir_ptr = core::ptr::null_mut();
        self.font_file_ptr = core::ptr::null_mut();
        // 释放 TTF 工作区 + 保存续读偏移（reading_task 随后 reboot_sleep 回正常模式）
        if let Some(ws) = self.ttf_ws.take() {
            unsafe {
                crate::ttf_sd::free_ws(ws as *mut _);
            }
        }
        unsafe {
            *core::ptr::addr_of_mut!(TTF_RESUME_OFFSET) = self.ttf_offset.min(self.ttf_file_len);
        }
        if let Some(display) = display_mut() {
            display.set_rotation(DisplayRotation::Rotate0);
        }
    }

    async fn bind_event(&mut self) {
        event::clear().await;

        // Key3 long: 退出当前态 / 退出阅读
        event::on_target(EventType::KeyLongEnd(3), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::JumpInput { .. }
                    | MenuState::Layout { .. }
                    | MenuState::FontList { .. }
                    | MenuState::BookmarkList { .. } => {
                        // 子菜单内长按 3：统一退回 Popup（取消当前操作）
                        mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                        mut_ref.bookmark_preview.clear();
                        mut_ref.need_render = true;
                    }
                    _ => {
                        if mut_ref.reading {
                            mut_ref.reading = false;
                            unsafe {
                                core::ptr::addr_of_mut!(PAGE_INDEX).write(None);
                            }
                            mut_ref.menu_state = MenuState::Closed;
                            mut_ref.need_render = true;
                        } else {
                            mut_ref.back().await;
                        }
                    }
                }
            });
        })
        .await;

        // Key3 short: 打开菜单 / 选择 / 确认
        event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { menu_index } => {
                        match menu_index {
                            0 => {
                                // 返回书单
                                mut_ref.reading = false;
                                unsafe {
                                    core::ptr::addr_of_mut!(PAGE_INDEX).write(None);
                                }
                            }
                            1 => {
                                mut_ref.save_bookmark_flag = true;
                            }
                            2 => {
                                mut_ref.menu_state = MenuState::BookmarkList { bm_index: 0, deleting: false };
                                mut_ref.need_load_preview = true;
                                mut_ref.need_render = true;
                                return;
                            }
                            3 => {
                                mut_ref.menu_state = MenuState::BookmarkList { bm_index: 0, deleting: true };
                                mut_ref.bookmark_preview.clear();
                                mut_ref.need_render = true;
                                return;
                            }
                            4 => {
                                // 跳转进度（百分比）
                                let pct = if mut_ref.ttf_file_len > 0 {
                                    mut_ref.ttf_offset.min(mut_ref.ttf_file_len) * 100 / mut_ref.ttf_file_len
                                } else {
                                    0
                                };
                                mut_ref.menu_state = MenuState::JumpInput { input_pct: pct as u8 };
                                mut_ref.jump_accel = 0;
                                mut_ref.need_render = true;
                                return;
                            }
                            5 => {
                                // 排版：快照当前值供“长按取消”还原
                                mut_ref.menu_state = MenuState::Layout {
                                    px: mut_ref.ttf_px as u8,
                                    gap: mut_ref.line_gap as u8,
                                };
                                mut_ref.need_render = true;
                                return;
                            }
                            6 => {
                                mut_ref.menu_state = MenuState::FontList { font_index: 0 };
                                mut_ref.need_render = true;
                                return;
                            }
                            7 => {
                                // 旋转屏幕
                                mut_ref.rotate = (mut_ref.rotate + 1) % 4;
                                super::set_rotation_state(mut_ref.rotate);
                                if let Some(display) = display_mut() {
                                    display.set_rotation(super::current_rotation());
                                }
                                mut_ref.save_reading_settings();
                            }
                            8 => {
                                crate::sleep::refresh_active_time().await;
                                crate::sleep::to_sleep_tips(Duration::from_secs(0), Duration::from_secs(0), true).await;
                                return;
                            }
                            _ => {} // 9 = 取消
                        }
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::JumpInput { input_pct } => {
                        // 确认跳转
                        let off = input_pct as u64 * mut_ref.ttf_file_len as u64 / 100;
                        mut_ref.jump_to_offset(off);
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::Layout { .. } => {
                        // 确认排版：保存到 flash 并关闭
                        mut_ref.save_reading_settings();
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::FontList { font_index } => {
                        // 确认字体
                        if let Some(name) = mut_ref.font_list.get(font_index as usize) {
                            mut_ref.font_name = name.clone();
                            mut_ref.save_reading_settings();
                            mut_ref.need_reload_font = true;
                        }
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::BookmarkList { bm_index, deleting } => {
                        let bm_count = mut_ref
                            .log_vec
                            .as_ref()
                            .map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 })
                            .unwrap_or(0) as u32;
                        if bm_index >= bm_count {
                            // 取消
                            mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                            mut_ref.bookmark_preview.clear();
                            mut_ref.need_render = true;
                        } else if deleting {
                            mut_ref.delete_bookmark_flag = true;
                            mut_ref.need_render = true;
                        } else if let Some(ref lv) = mut_ref.log_vec {
                            let bookmarks: Vec<u32, LOG_VEC_MAX> = lv.iter().skip(1).copied().collect();
                            if (bm_index as usize) < bookmarks.len() {
                                mut_ref.jump_to_offset(bookmarks[bm_index as usize] as u64);
                            }
                            mut_ref.menu_state = MenuState::Closed;
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                            mut_ref.need_render = true;
                        } else if mut_ref.exit_selected {
                            mut_ref.back().await;
                        } else {
                            // 打开选中书：从头/上次位置开始
                            mut_ref.reading = true;
                            mut_ref.ttf_file_len = 0;
                            mut_ref.ttf_offset = 0;
                            mut_ref.ttf_end = 0;
                            unsafe {
                                core::ptr::addr_of_mut!(PAGE_INDEX).write(Some(mut_ref.choose_index));
                                // 新书从头（清除上一本续读偏移）
                                core::ptr::addr_of_mut!(TTF_RESUME_OFFSET).write(0);
                            }
                            mut_ref.need_render = true;
                        }
                    }
                }
            });
        })
        .await;

        // Key1 long hold: 向下滚动 / 加速跳转 / 下一项
        event::on_target(EventType::KeyLongIng(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        *menu_index = (*menu_index + 1) % MENU_ITEMS.len() as u32;
                        mut_ref.need_render = true;
                        Timer::after_millis(200).await;
                    }
                    MenuState::JumpInput { ref mut input_pct } => {
                        let step = accel_step(mut_ref.jump_accel);
                        mut_ref.jump_accel += 1;
                        let np = (*input_pct as u32).saturating_add(step).min(100);
                        *input_pct = np as u8;
                        mut_ref.need_render = true;
                        Timer::after_millis(75).await;
                    }
                    MenuState::Layout { .. } => {
                        // 长按：行距 +1（实时）
                        mut_ref.line_gap = (mut_ref.line_gap + 1).min(10);
                        mut_ref.need_render = true;
                        Timer::after_millis(150).await;
                    }
                    MenuState::FontList { ref mut font_index } => {
                        let max = mut_ref.font_list.len() as u32;
                        if max > 0 {
                            *font_index = (*font_index + 1) % max;
                            mut_ref.need_render = true;
                            Timer::after_millis(150).await;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        let bm_count = mut_ref
                            .log_vec
                            .as_ref()
                            .map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 })
                            .unwrap_or(0) as u32;
                        if *bm_index < bm_count {
                            *bm_index += 1;
                            if !deleting {
                                mut_ref.need_load_preview = true;
                            }
                            mut_ref.need_render = true;
                            Timer::after_millis(200).await;
                        }
                    }
                    MenuState::Closed => {
                        if !mut_ref.reading {
                            if mut_ref.exit_selected {
                                mut_ref.exit_selected = false;
                                mut_ref.choose_index = 0;
                            } else {
                                let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                                if max > 0 && mut_ref.choose_index < (max - 1) as u32 {
                                    mut_ref.choose_index += 1;
                                } else if max > 0 {
                                    mut_ref.exit_selected = true;
                                }
                            }
                            display::reset_render_times();
                            mut_ref.need_render = true;
                            Timer::after_millis(200).await;
                        }
                    }
                }
            });
        })
        .await;

        // Key2 long hold: 向上滚动 / 加速跳转 / 上一项
        event::on_target(EventType::KeyLongIng(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        *menu_index = (*menu_index + MENU_ITEMS.len() as u32 - 1) % MENU_ITEMS.len() as u32;
                        mut_ref.need_render = true;
                        Timer::after_millis(200).await;
                    }
                    MenuState::JumpInput { ref mut input_pct } => {
                        let step = accel_step(mut_ref.jump_accel);
                        mut_ref.jump_accel += 1;
                        let np = (*input_pct as u32).saturating_sub(step);
                        *input_pct = np as u8;
                        mut_ref.need_render = true;
                        Timer::after_millis(75).await;
                    }
                    MenuState::Layout { .. } => {
                        mut_ref.line_gap = (mut_ref.line_gap - 1).max(0);
                        mut_ref.need_render = true;
                        Timer::after_millis(150).await;
                    }
                    MenuState::FontList { ref mut font_index } => {
                        let max = mut_ref.font_list.len() as u32;
                        if max > 0 {
                            *font_index = (*font_index + max - 1) % max;
                            mut_ref.need_render = true;
                            Timer::after_millis(150).await;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        if *bm_index > 0 {
                            *bm_index -= 1;
                            if !deleting {
                                mut_ref.need_load_preview = true;
                            }
                            mut_ref.need_render = true;
                            Timer::after_millis(200).await;
                        }
                    }
                    MenuState::Closed => {
                        if !mut_ref.reading {
                            if mut_ref.exit_selected {
                                let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                                if max > 0 {
                                    mut_ref.exit_selected = false;
                                    mut_ref.choose_index = (max - 1) as u32;
                                    display::reset_render_times();
                                    mut_ref.need_render = true;
                                    Timer::after_millis(200).await;
                                }
                            } else if mut_ref.choose_index > 0 {
                                mut_ref.choose_index -= 1;
                                display::reset_render_times();
                                mut_ref.need_render = true;
                                Timer::after_millis(200).await;
                            } else {
                                mut_ref.exit_selected = true;
                                display::reset_render_times();
                                mut_ref.need_render = true;
                                Timer::after_millis(200).await;
                            }
                        }
                    }
                }
            });
        })
        .await;

        // Key1 short: 向下 / +1 / 下一页 / 下一本
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        *menu_index = (*menu_index + 1) % MENU_ITEMS.len() as u32;
                        mut_ref.need_render = true;
                    }
                    MenuState::JumpInput { ref mut input_pct } => {
                        *input_pct = (*input_pct + 1).min(100);
                        mut_ref.jump_accel = 0;
                        mut_ref.need_render = true;
                    }
                    MenuState::Layout { .. } => {
                        // 字号 +2（实时：改值 + 清字形缓存 + 重渲染）
                        let np = (mut_ref.ttf_px + 2.0).min(40.0);
                        mut_ref.ttf_px = np;
                        if let Some(ws) = mut_ref.ttf_ws.as_mut() {
                            crate::ttf_sd::cache_clear(ws);
                        }
                        mut_ref.need_render = true;
                    }
                    MenuState::FontList { ref mut font_index } => {
                        let max = mut_ref.font_list.len() as u32;
                        if max > 0 {
                            *font_index = (*font_index + 1) % max;
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        let bm_count = mut_ref
                            .log_vec
                            .as_ref()
                            .map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 })
                            .unwrap_or(0) as u32;
                        if *bm_index < bm_count {
                            *bm_index += 1;
                            if !deleting {
                                mut_ref.need_load_preview = true;
                            }
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            mut_ref.do_change_page(true).await;
                        } else if mut_ref.exit_selected {
                            mut_ref.exit_selected = false;
                            mut_ref.choose_index = 0;
                            mut_ref.need_render = true;
                        } else {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 {
                                if mut_ref.choose_index < (max - 1) as u32 {
                                    mut_ref.choose_index += 1;
                                } else {
                                    mut_ref.exit_selected = true;
                                }
                            }
                            mut_ref.need_render = true;
                        }
                    }
                }
            });
        })
        .await;

        // Key2 short: 向上 / -1 / 上一页 / 上一本
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.menu_state {
                    MenuState::Popup { ref mut menu_index } => {
                        *menu_index = (*menu_index + MENU_ITEMS.len() as u32 - 1) % MENU_ITEMS.len() as u32;
                        mut_ref.need_render = true;
                    }
                    MenuState::JumpInput { ref mut input_pct } => {
                        *input_pct = input_pct.saturating_sub(1);
                        mut_ref.jump_accel = 0;
                        mut_ref.need_render = true;
                    }
                    MenuState::Layout { .. } => {
                        let np = (mut_ref.ttf_px - 2.0).max(12.0);
                        mut_ref.ttf_px = np;
                        if let Some(ws) = mut_ref.ttf_ws.as_mut() {
                            crate::ttf_sd::cache_clear(ws);
                        }
                        mut_ref.need_render = true;
                    }
                    MenuState::FontList { ref mut font_index } => {
                        let max = mut_ref.font_list.len() as u32;
                        if max > 0 {
                            *font_index = (*font_index + max - 1) % max;
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index, deleting } => {
                        if *bm_index > 0 {
                            *bm_index -= 1;
                            if !deleting {
                                mut_ref.need_load_preview = true;
                            }
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            mut_ref.do_change_page(false).await;
                        } else if mut_ref.exit_selected {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 {
                                mut_ref.exit_selected = false;
                                mut_ref.choose_index = (max - 1) as u32;
                            }
                            mut_ref.need_render = true;
                        } else {
                            if mut_ref.choose_index > 0 {
                                mut_ref.choose_index -= 1;
                            } else {
                                mut_ref.exit_selected = true;
                            }
                            mut_ref.need_render = true;
                        }
                    }
                }
            });
        })
        .await;
    }
}
