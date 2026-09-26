use alloc::boxed::Box;
use alloc::format;
use core::str::FromStr;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::primitives::rounded_rectangle::{RoundedRectangle, CornerRadii};
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
use crate::widgets::battery::BODY_H;
use core::ops::ControlFlow;
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

/// 字体列表容量。末尾固定追加 3 个内置点阵字体，扫描 TTF 时预留这 3 位
/// （TTF 最多收 FONT_LIST_CAP-3 个），避免 TTF 占满把点阵挤掉（push 静默失败）。
const FONT_LIST_CAP: usize = 16;

/// 内置点阵字体名 → 单元格像素（wqy12→12 … wqy16→16）。None 表示非点阵（走 TTF）。
/// 点阵用 u8g2 内置 wqy 位图，不读 SD 字库、无光栅化，渲染比 TTF 快。
fn bm_size_of(name: &str) -> Option<u32> {
    match name {
        "点阵16" => Some(16),
        "点阵14" => Some(14),
        "点阵12" => Some(12),
        _ => None,
    }
}

/// 当前字体是否为点阵。
fn is_bitmap(name: &str) -> bool {
    bm_size_of(name).is_some()
}

/// 按 wqy16 像素宽度（中文16、英文/数字8）截断字符串，超出加 "…"。
/// 用于书单：长书名在窄屏（2.7寸）会延伸到右侧与进度"%"重叠。
/// 只截断显示用副本，打开书仍用 menus 里的全名。
fn truncate_name(name: &str, max_w: i32) -> alloc::string::String {
    const FULL: i32 = 16;
    const HALF: i32 = 8;
    let mut w = 0i32;
    let mut out: alloc::string::String = alloc::string::String::new();
    let mut cut = false;
    for ch in name.chars() {
        let cw = if (ch as u32) < 0x80 { HALF } else { FULL };
        if w + cw > max_w {
            cut = true;
            break;
        }
        w += cw;
        let _ = out.push(ch);
    }
    if cut {
        // 腾出 "…"（按全宽计）的位置
        while w + FULL > max_w {
            if out.pop().is_some() {
                w -= FULL; // 近似（末字可能半宽，偏保守，宁短勿 overlap）
            } else {
                break;
            }
        }
        let _ = out.push('…');
    }
    out
}

// 菜单项：0=返回书单 1=收藏书签 2=打开书签 3=删除书签 4=跳转进度
//         5=排版 6=字体 7=旋转屏幕 8=睡眠 9=方向键互换 else=取消
const MENU_ITEMS: &[&str] = &[
    "返回书单", "收藏书签", "打开书签", "删除书签", "跳转进度",
    "排版", "字体", "旋转屏幕", "睡眠", "方向键互换", "取消",
];
/// “方向键互换”项的菜单索引（动态显示 开/关 状态）
const MENU_IDX_SWAP: usize = 9;

enum MenuState {
    Closed,
    Popup { menu_index: u32 },
    JumpInput { input_pct: u8 },
    /// 排版：cursor 0=字号 1=行距 2=返回；editing=true 时上下键改值。
    Layout { cursor: u8, editing: bool },
    FontList { font_index: u32 },
    BookmarkList { bm_index: u32, deleting: bool },
}

/// 菜单状态→id（只区分变体，不区分内部 cursor/index）。
/// render 用它检测"跨状态切换"：不同状态面板几何不同，只局部重绘叠层会留下旧面板残影，故需全量重绘。
fn menu_state_id(s: &MenuState) -> u8 {
    match s {
        MenuState::Closed => 0,
        MenuState::Popup { .. } => 1,
        MenuState::JumpInput { .. } => 2,
        MenuState::Layout { .. } => 3,
        MenuState::FontList { .. } => 4,
        MenuState::BookmarkList { .. } => 5,
    }
}

pub struct ReadPage {
    running: bool,
    reading: bool,
    need_render: bool,
    /// 本次渲染是否需要重绘正文（清屏 + TTF 光栅化）。
    /// 菜单打开时仅旋转/排版改值才置 true；同状态内移动光标保持 false → 只重绘菜单叠层。
    need_text_render: bool,
    /// 上次渲染时的菜单状态 id（检测跨状态切换：面板几何不同需全量重绘避免残影）。
    last_menu_state_id: u8,
    choose_index: u32,
    menus: Option<Vec<String<BOOK_NAME_MAX>, 40>>,
    log_vec: Option<Vec<u32, LOG_VEC_MAX>>,
    menu_state: MenuState,
    save_bookmark_flag: bool,
    delete_bookmark_flag: bool,
    /// 翻页/跳转后写 .log[0]（持久化阅读位置到 SD，防掉电丢失）。
    need_save_position: bool,
    need_load_preview: bool,
    /// 返回书单时需重载书单进度（读到最新 .log[0]）。
    need_reload_progress: bool,
    bookmark_preview: String<ONE_PAGE_CONTENT_LEN>,
    /// 屏幕旋转状态（0..3，相对默认依次向右 +90°）。
    rotate: u8,
    /// 阅读功能内按键 1/2 方向互换（翻页/菜单/列表全换，flash 持久化）。
    swap_keys: bool,
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
    /// 后退历史栈：访问过的页起始偏移（栈顶=最近一页）。前进时压栈，后退时弹栈，O(1)。
    /// 栈空时（刚进入/跳转后第一次后退）退化为局部逆推 find_prev_start（只回扫几页）。
    /// 不预建全书索引——动态分页前进天生 O(1)，后退靠历史栈，进书零延迟。
    ttf_hist: Vec<u32, 256>,
    /// 当前页已是最后一页（渲染时由 offset+consumed>=file_len 判定）。
    ttf_at_end: bool,
    /// 在最后一页仍按前进时显示一次"已是最后一页"提示（渲染后清）。
    ttf_last_hint: bool,
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
    /// SD 卡上可用的字体列表（TTF 扫描 + 3 个内置点阵，进 run 时扫描一次）。
    font_list: Vec<String<32>, FONT_LIST_CAP>,
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

    /// 方向键短按统一入口（down=Key1 语义：向下/+1/下一页/下一本）。
    /// “方向键互换”开启时由 bind_event 把两条物理键的 down 取值对调，
    /// 阅读功能内（翻页/菜单/跳转/排版/字体/书签/书单）方向整体互换。
    async fn dir_key_short(&mut self, down: bool) {
        match self.menu_state {
            MenuState::Popup { ref mut menu_index } => {
                *menu_index = if down {
                    (*menu_index + 1) % MENU_ITEMS.len() as u32
                } else {
                    (*menu_index + MENU_ITEMS.len() as u32 - 1) % MENU_ITEMS.len() as u32
                };
                self.need_render = true;
            }
            MenuState::JumpInput { ref mut input_pct } => {
                if down {
                    *input_pct = (*input_pct + 1).min(100);
                } else {
                    *input_pct = input_pct.saturating_sub(1);
                }
                self.jump_accel = 0;
                self.need_render = true;
            }
            MenuState::Layout { ref mut cursor, ref mut editing } => {
                if *editing {
                    match *cursor {
                        0 => {
                            let np = if down {
                                (self.ttf_px - 1.0).max(12.0)
                            } else {
                                (self.ttf_px + 1.0).min(40.0)
                            };
                            self.ttf_px = np;
                            if let Some(ws) = self.ttf_ws.as_mut() {
                                crate::ttf_sd::cache_clear(ws, np);
                            }
                        }
                        1 => {
                            self.line_gap = if down {
                                (self.line_gap - 1).max(0)
                            } else {
                                (self.line_gap + 1).min(10)
                            };
                        }
                        _ => {}
                    }
                    self.need_text_render = true;
                } else {
                    *cursor = if down {
                        (*cursor + 1) % 3
                    } else {
                        (*cursor + 3 - 1) % 3
                    };
                }
                self.need_render = true;
            }
            MenuState::FontList { ref mut font_index } => {
                let max = self.font_list.len() as u32;
                if max > 0 {
                    *font_index = if down {
                        (*font_index + 1) % max
                    } else {
                        (*font_index + max - 1) % max
                    };
                    self.need_render = true;
                }
            }
            MenuState::BookmarkList { ref mut bm_index, .. } => {
                let bm_count = self
                    .log_vec
                    .as_ref()
                    .map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 })
                    .unwrap_or(0) as u32;
                if (down && *bm_index < bm_count) || (!down && *bm_index > 0) {
                    if down {
                        *bm_index += 1;
                    } else {
                        *bm_index -= 1;
                    }
                    self.need_load_preview = true;
                    self.need_render = true;
                }
            }
            MenuState::Closed => {
                if self.reading {
                    self.do_change_page(down).await;
                } else if down {
                    if self.exit_selected {
                        self.exit_selected = false;
                        self.choose_index = 0;
                    } else {
                        let max = self.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                        if max > 0 {
                            if self.choose_index < (max - 1) as u32 {
                                self.choose_index += 1;
                            } else {
                                self.exit_selected = true;
                            }
                        }
                    }
                    self.need_render = true;
                } else {
                    if self.exit_selected {
                        let max = self.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                        if max > 0 {
                            self.exit_selected = false;
                            self.choose_index = (max - 1) as u32;
                        }
                    } else if self.choose_index > 0 {
                        self.choose_index -= 1;
                    } else {
                        self.exit_selected = true;
                    }
                    self.need_render = true;
                }
            }
        }
    }

    /// 方向键长按连发统一入口（down=Key1 语义）。注意：排版编辑时长按的
    /// 字号/行距增减方向与短按相反——原有行为，保持不变。
    async fn dir_key_long(&mut self, down: bool) {
        match self.menu_state {
            MenuState::Popup { ref mut menu_index } => {
                *menu_index = if down {
                    (*menu_index + 1) % MENU_ITEMS.len() as u32
                } else {
                    (*menu_index + MENU_ITEMS.len() as u32 - 1) % MENU_ITEMS.len() as u32
                };
                self.need_render = true;
                Timer::after_millis(200).await;
            }
            MenuState::JumpInput { ref mut input_pct } => {
                let step = accel_step(self.jump_accel);
                self.jump_accel += 1;
                if down {
                    let np = (*input_pct as u32).saturating_add(step).min(100);
                    *input_pct = np as u8;
                } else {
                    let np = (*input_pct as u32).saturating_sub(step);
                    *input_pct = np as u8;
                }
                self.need_render = true;
                Timer::after_millis(75).await;
            }
            MenuState::Layout { ref cursor, ref editing } => {
                if *editing {
                    match *cursor {
                        0 => {
                            let np = if down {
                                (self.ttf_px + 1.0).min(40.0)
                            } else {
                                (self.ttf_px - 1.0).max(12.0)
                            };
                            self.ttf_px = np;
                            if let Some(ws) = self.ttf_ws.as_mut() {
                                crate::ttf_sd::cache_clear(ws, np);
                            }
                        }
                        1 => {
                            self.line_gap = if down {
                                (self.line_gap + 1).min(10)
                            } else {
                                (self.line_gap - 1).max(0)
                            };
                        }
                        _ => {}
                    }
                    self.need_text_render = true;
                    self.need_render = true;
                    Timer::after_millis(150).await;
                }
            }
            MenuState::FontList { ref mut font_index } => {
                let max = self.font_list.len() as u32;
                if max > 0 {
                    *font_index = if down {
                        (*font_index + 1) % max
                    } else {
                        (*font_index + max - 1) % max
                    };
                    self.need_render = true;
                    Timer::after_millis(150).await;
                }
            }
            MenuState::BookmarkList { ref mut bm_index, .. } => {
                let bm_count = self
                    .log_vec
                    .as_ref()
                    .map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 })
                    .unwrap_or(0) as u32;
                if (down && *bm_index < bm_count) || (!down && *bm_index > 0) {
                    if down {
                        *bm_index += 1;
                    } else {
                        *bm_index -= 1;
                    }
                    self.need_load_preview = true;
                    self.need_render = true;
                    Timer::after_millis(200).await;
                }
            }
            MenuState::Closed => {
                if !self.reading {
                    if down {
                        if self.exit_selected {
                            self.exit_selected = false;
                            self.choose_index = 0;
                        } else {
                            let max = self.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 && self.choose_index < (max - 1) as u32 {
                                self.choose_index += 1;
                            } else if max > 0 {
                                self.exit_selected = true;
                            }
                        }
                        display::reset_render_times();
                        self.need_render = true;
                        Timer::after_millis(200).await;
                    } else if self.exit_selected {
                        let max = self.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                        if max > 0 {
                            self.exit_selected = false;
                            self.choose_index = (max - 1) as u32;
                            display::reset_render_times();
                            self.need_render = true;
                            Timer::after_millis(200).await;
                        }
                    } else if self.choose_index > 0 {
                        self.choose_index -= 1;
                        display::reset_render_times();
                        self.need_render = true;
                        Timer::after_millis(200).await;
                    } else {
                        self.exit_selected = true;
                        display::reset_render_times();
                        self.need_render = true;
                        Timer::after_millis(200).await;
                    }
                }
            }
        }
    }

    /// 跳到指定字节偏移（跳转进度/书签）。清历史、保存续读偏移、触发渲染。
    fn jump_to_offset(&mut self, off: u64) {
        let off = off.min(self.ttf_file_len as u64) as u32;
        // 大跳转后历史失效，清空；后退退化为局部逆推。
        self.ttf_hist.clear();
        self.ttf_offset = off;
        self.ttf_end = off;
        self.ttf_at_end = false;
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
        self.ttf_hist.clear();
    }

    /// 读 .log 进 log_vec。ttf_resume：跳过 .log[0] 还原（rtc_fast 权威），清 ttf_resume。
    /// 否则：用 .log[0] 还原 ttf_offset（夹到 file_len）。
    async fn get_log_vec(&mut self, books_dir: &mut ActualDirectory<'_>) {
        let book_name = match self.menus.as_ref().and_then(|m| m.get(self.choose_index as usize)) {
            Some(n) => n.clone(),
            None => return,
        };
        let log_file = SdMount::open_log_file(books_dir, &book_name, embedded_sdmmc::Mode::ReadOnly);
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
            if let Some(&raw) = lv.first() {
                // bit31 = 是否已到末页（完成标记），低 31 位才是字节偏移
                let off = raw & 0x7FFF_FFFF;
                self.ttf_offset = off.min(self.ttf_file_len);
                self.ttf_end = self.ttf_offset;
            }
        }
        // 同步续读偏移到 rtc_fast：打开书时 TTF_RESUME_OFFSET 被清 0、由 .log[0] 还原了
        // ttf_offset，此处补回，保证"睡眠唤醒"(用 TTF_RESUME_OFFSET) 与当前页一致，
        // 否则未翻页直接睡眠会回到第一页。
        unsafe {
            *core::ptr::addr_of_mut!(TTF_RESUME_OFFSET) = self.ttf_offset;
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
                        let raw = match SdMount::open_log_file(books_dir, book_name, embedded_sdmmc::Mode::ReadOnly) {
                            Ok(mut f) => {
                                let log = TxtReader::read_log(&mut f);
                                f.close();
                                log.first().copied().unwrap_or(0)
                            }
                            Err(_) => 0,
                        };
                        (raw, total)
                    }
                    None => {
                        let _ = self.book_progress.push(String::new());
                        continue;
                    }
                };
                let mut s: String<16> = String::new();
                if total > 0 {
                    use core::fmt::Write;
                    // bit31 = 已到末页（完成标记）→ 直接 100%；否则按字节偏移两位小数
                    if offset & (1u32 << 31) != 0 {
                        let _ = write!(s, "100.00%");
                    } else {
                        let p = (offset as u64) * 10000 / total as u64;
                        let _ = write!(s, "{}.{:02}%", p / 100, p % 100);
                    }
                }
                let _ = self.book_progress.push(s);
            }
        }
    }

    /// TTF 翻页。前进 O(1)（offset←end）；后退 O(1)（历史栈弹栈），
    /// 栈空时（刚进入/跳转后第一次后退）局部逆推上一页起始，只回扫几页，绝不扫全书。
    async fn do_change_page(&mut self, next: bool) {
        if next {
            // 前进：已在最后一页则提示一次，不前进
            if self.ttf_at_end {
                self.ttf_last_hint = true;
                self.need_render = true;
                return;
            }
            // 栈满丢最老记录，保持"最近 256 页"语义：否则满后 push 静默失败，
            // 后退会一直弹到会话早期读过的老位置（长时间连续阅读后偶现的大幅回跳）。
            if self.ttf_hist.is_full() {
                self.ttf_hist.remove(0);
            }
            let _ = self.ttf_hist.push(self.ttf_offset);
            self.ttf_offset = self.ttf_end;
        } else {
            // 后退：历史栈优先（精确，与前进对称）
            if let Some(prev) = self.ttf_hist.pop() {
                if prev >= self.ttf_offset {
                    return; // 已到开头，翻不动
                }
                self.ttf_offset = prev;
            } else {
                // 栈空：局部逆推上一页起始（近似，回扫几页）
                let prev = self.find_prev_page_start();
                if prev >= self.ttf_offset {
                    return; // 已在开头
                }
                self.ttf_offset = prev;
            }
            self.ttf_at_end = false;
        }
        self.ttf_end = self.ttf_offset;
        unsafe {
            *core::ptr::addr_of_mut!(TTF_RESUME_OFFSET) = self.ttf_offset;
        }
        self.need_save_position = true;
        self.need_render = true;
    }

    /// 局部逆推当前页的上一页起始偏移（历史栈空时用）：从 cur 往前回退一段，
    /// 逐页 compute_page_consumed 扫到 cur，取最后那个 < cur 的页起始。只扫几页，不扫全书。
    fn find_prev_page_start(&mut self) -> u32 {
        let cur = self.ttf_offset;
        if cur == 0 || self.ttf_file_len == 0 {
            return 0;
        }
        let bm_cell = bm_size_of(self.font_name.as_str());
        // 点阵无需 font_file_ptr；TTF 需要句柄
        if bm_cell.is_none() && self.font_file_ptr.is_null() {
            return 0;
        }
        let book_name = match self
            .menus
            .as_ref()
            .and_then(|m| m.get(self.choose_index as usize))
            .cloned()
        {
            Some(n) => n,
            None => return 0,
        };
        let bd_ptr = self.books_dir_ptr;
        let font_ptr = self.font_file_ptr;
        let ttf_px = self.ttf_px;
        let line_gap = self.line_gap;
        let file_len = self.ttf_file_len;
        let Some(ws) = self.ttf_ws.as_mut() else { return 0; };
        if bd_ptr.is_null() {
            return 0;
        }
        let bd: &mut ActualDirectory<'_> = unsafe { &mut *bd_ptr };
        let file_name = format!("{}.txt", book_name);
        let Some(sn) = SdMount::find_entry_by_name(bd, &file_name).map(|e| e.name) else {
            return 0;
        };
        let Ok(mut bf) = bd.open_file_in_dir(sn, embedded_sdmmc::Mode::ReadOnly) else {
            return 0;
        };
        let text_area_h =
            super::visual_height() as i32 - super::PROGRESS_AREA_HEIGHT as i32 - 2;
        let mw = super::text_width() as i32;
        let prev = if let Some(cell) = bm_cell {
            // 点阵局部逆推（纯算术，无字体 I/O）
            let full_w = cell as i32;
            let half_w = (cell / 2) as i32;
            let base_h = (cell as i32 + line_gap).max(1);
            let max_lines = ((text_area_h / base_h) as u32).max(1);
            Some(super::bitmap::find_prev_start_bitmap(
                &mut bf, &mut ws.book_buf, cur, file_len, mw, full_w, half_w, max_lines,
            ))
        } else {
            let ff: &mut ActualFile = unsafe { &mut *font_ptr };
            match crate::ttf_sd::open_font(ff, ws) {
                Some(fi) => {
                    let base_h = (fi.line_height(ttf_px) + line_gap).max(1);
                    let max_lines = ((text_area_h / base_h) as u32).max(1);
                    let line_h = text_area_h / max_lines as i32;
                    Some(crate::ttf_sd::find_prev_start(
                        &mut bf, ff, &fi, ws, cur, file_len, ttf_px, mw, line_h, max_lines,
                    ))
                }
                None => None,
            }
        };
        bf.close();
        prev.unwrap_or(0)
    }

    /// 预加载下一页字形：渲染完当前页后提前光栅化进缓存（不画），下次翻页直接 blit。
    async fn preload_next_page(&mut self) {
        // 点阵无需光栅化（位图直接 blit），跳过预加载
        if is_bitmap(self.font_name.as_str()) {
            return;
        }
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
        let vw = super::visual_width();
        let vh = super::visual_height();
        // 小屏（如2.7寸横屏176高）放不下 10项×24 的菜单：按屏高自适应行高与字号。
        let compact = vh < 200;
        let page_info_height: u32 = if compact { 12 } else { 18 };
        let menu_padding: u32 = if compact { 4 } else { 8 };
        let avail = vh.saturating_sub(page_info_height + menu_padding * 2);
        let menu_item_height: u32 = (avail / MENU_ITEMS.len() as u32).clamp(12, 24);
        let font14: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>()
            .with_ignore_unknown_chars(true);
        let font12: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312>()
            .with_ignore_unknown_chars(true);
        let font = if menu_item_height < 18 { &font12 } else { &font14 };
        let menu_width: u32 = if vw < 200 { vw - 16 } else { 180 };

        // 圆角美化：菜单面板与选中高亮统一使用圆角（小屏用小半径）。
        let corner: u32 = if compact { 3 } else { 6 };
        let corners = CornerRadii::new(Size::new_equal(corner));

        let box_style = PrimitiveStyleBuilder::new()
            .fill_color(White)
            .stroke_color(Black)
            .stroke_alignment(StrokeAlignment::Outside)
            .stroke_width(2)
            .build();

        // 电量：仅主菜单(Popup)底部进度行左侧显示，子菜单不显示
        let bat_pct = crate::battery::last_percent();

        match self.menu_state {
            MenuState::Popup { menu_index } => {
                let page_info_height: u32 = 18;
                let menu_height = MENU_ITEMS.len() as u32 * menu_item_height + page_info_height + menu_padding * 2;
                let menu_x = ((vw - menu_width) / 2) as i32;
                let menu_y = ((vh - menu_height) / 2) as i32;

                RoundedRectangle::new(
                    Rectangle::new(Point::new(menu_x, menu_y), Size::new(menu_width, menu_height)),
                    corners,
                )
                .into_styled(box_style)
                .draw(display)
                .ok();

                // “方向键互换”项动态拼接当前开关状态，其余项用静态文案
                let labels: [&str; MENU_ITEMS.len()] = core::array::from_fn(|i| {
                    if i == MENU_IDX_SWAP {
                        if self.swap_keys { "方向键互换: 开" } else { "方向键互换: 关" }
                    } else {
                        MENU_ITEMS[i]
                    }
                });
                for (i, label) in labels.iter().enumerate() {
                    let item_y = menu_y + menu_padding as i32 + (i as u32 * menu_item_height) as i32;
                    let is_selected = i as u32 == menu_index;
                    if is_selected {
                        RoundedRectangle::new(
                            Rectangle::new(
                                Point::new(menu_x + 4, item_y),
                                Size::new(menu_width - 8, menu_item_height),
                            ),
                            corners,
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
                    // 进度按 ttf_offset；ttf_offset+一页(ttf_end) ≥ file_len 即最后一页 → 算 100%
                    let cur = if self.ttf_end >= self.ttf_file_len {
                        self.ttf_file_len
                    } else {
                        self.ttf_offset.min(self.ttf_file_len)
                    };
                    let p = cur as u64 * 10000 / self.ttf_file_len as u64;
                    let page_text_y = menu_y + menu_height as i32 - menu_padding as i32;
                    font.render_aligned(
                        format_args!("{}.{:02}%", p / 100, p % 100),
                        Point::new(menu_x + menu_width as i32 - menu_padding as i32, page_text_y),
                        VerticalPosition::Bottom,
                        HorizontalAlignment::Right,
                        FontColor::Transparent(Black),
                        display,
                    )
                    .ok();
                    // 左侧同排显示电量
                    if bat_pct > 0 {
                        let bat_y = page_text_y - BODY_H as i32;
                        let _ = crate::widgets::battery::draw_battery(
                            bat_pct,
                            Point::new(menu_x + menu_padding as i32, bat_y),
                            Black,
                            &font12,
                            display,
                        );
                    }
                } else if bat_pct > 0 {
                    // 无进度信息（未打开书）时电量单独占底行左侧
                    let bat_y = menu_y + menu_height as i32 - menu_padding as i32 - BODY_H as i32;
                    let _ = crate::widgets::battery::draw_battery(
                        bat_pct,
                        Point::new(menu_x + menu_padding as i32, bat_y),
                        Black,
                        &font12,
                        display,
                    );
                }
            }
            MenuState::JumpInput { input_pct } => {
                let jump_height: u32 = 100;
                let jump_x = ((vw - menu_width) / 2) as i32;
                let jump_y = ((vh - jump_height) / 2) as i32;
                let center_x = (vw / 2) as i32;

                RoundedRectangle::new(
                    Rectangle::new(Point::new(jump_x, jump_y), Size::new(menu_width, jump_height)),
                    corners,
                )
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
            MenuState::Layout { cursor, editing } => {
                let h: u32 = 130;
                let x = ((vw - menu_width) / 2) as i32;
                let y = ((vh - h) / 2) as i32;
                let cx = x + menu_width as i32 / 2;
                let row_h: i32 = 22;
                let y0 = y + 26; // 第一行顶

                RoundedRectangle::new(
                    Rectangle::new(Point::new(x, y), Size::new(menu_width, h)),
                    corners,
                )
                .into_styled(box_style)
                .draw(display)
                .ok();

                font.render_aligned(
                    "排版设置",
                    Point::new(cx, y + 15),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                )
                .ok();

                // 三行选项：0=字号 1=行距 2=返回
                let items: [(&str, heapless::String<24>); 3] = [
                    ("字号", {
                        let mut s: heapless::String<24> = heapless::String::new();
                        let _ = core::fmt::write(&mut s, format_args!("{}", self.ttf_px as i32));
                        s
                    }),
                    ("行距", {
                        let mut s: heapless::String<24> = heapless::String::new();
                        let _ = core::fmt::write(&mut s, format_args!("{}", self.line_gap));
                        s
                    }),
                    ("返回", heapless::String::new()),
                ];
                for (i, (label, val)) in items.iter().enumerate() {
                    let ry = y0 + i as i32 * row_h;
                    let is_sel = i as u8 == cursor;
                    // 选中行反相条
                    if is_sel {
                        let strip = Rectangle::new(
                            Point::new(x + 4, ry),
                            Size::new(menu_width - 8, row_h as u32 - 2),
                        );
                        let _ = strip.into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build()).draw(display);
                    }
                    let lbl_color = if is_sel { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                    font.render_aligned(
                        *label,
                        Point::new(x + 12, ry + row_h / 2),
                        VerticalPosition::Center, HorizontalAlignment::Left,
                        lbl_color, display,
                    )
                    .ok();
                    if !val.is_empty() {
                        let prefix = if editing && is_sel { ">> " } else { "" };
                        font.render_aligned(
                            format_args!("{}{}", prefix, val.as_str()),
                            Point::new(x + menu_width as i32 - 12, ry + row_h / 2),
                            VerticalPosition::Center, HorizontalAlignment::Right,
                            lbl_color, display,
                        )
                        .ok();
                    }
                }
                // 底部提示
                let tip = if editing { "1/2改值 3退出编辑" } else { "1/2选择 3确认 长按退出" };
                font.render_aligned(
                    tip,
                    Point::new(cx, y + h as i32 - 12),
                    VerticalPosition::Center, HorizontalAlignment::Center,
                    FontColor::Transparent(Black), display,
                )
                .ok();
            }
            MenuState::FontList { font_index } => {
                let count = self.font_list.len() as u32;
                let h = vh - 20;
                let x = ((vw - menu_width) / 2) as i32;
                let y: i32 = 10;
                let cx = x + menu_width as i32 / 2;

                RoundedRectangle::new(
                    Rectangle::new(Point::new(x, y), Size::new(menu_width, h)),
                    corners,
                )
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
                        RoundedRectangle::new(
                            Rectangle::new(
                                Point::new(x + 3, iy),
                                Size::new(menu_width - 6, item_h as u32),
                            ),
                            corners,
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
                // 列表为主体（原 list=1/3 时只看得到 1 个书签），预览缩到约 1/3
                let preview_h = total_h / 3;
                let list_h = total_h - preview_h - gap;
                let list_x = ((vw as i32 - menu_width as i32) / 2) as i32;
                let list_y = margin;
                let list_right = list_x + menu_width as i32;
                let preview_x: i32 = 0;
                let preview_y = margin + list_h + gap;

                RoundedRectangle::new(
                    Rectangle::new(Point::new(list_x, list_y), Size::new(menu_width, list_h as u32)),
                    corners,
                )
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
                    RoundedRectangle::new(
                        Rectangle::new(Point::new(list_x + 3, cancel_y), Size::new(menu_width - 6, menu_item_height)),
                        corners,
                    )
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
                        RoundedRectangle::new(
                            Rectangle::new(
                                Point::new(list_x + 3, item_y),
                                Size::new((text_right - list_x - 3) as u32, menu_item_height),
                            ),
                            corners,
                        )
                        .into_styled(PrimitiveStyleBuilder::new().fill_color(Black).build())
                        .draw(display)
                        .ok();
                    }
                    let text_color = if is_selected { FontColor::Transparent(White) } else { FontColor::Transparent(Black) };
                    let prefix = if is_selected { "> " } else { "  " };
                    let delete_mark = if deleting && is_selected { " ×" } else { "" };
                    let p = if self.ttf_file_len > 0 { off as u64 * 10000 / self.ttf_file_len as u64 } else { 0 };
                    font.render_aligned(
                        format_args!("{}{}.{:02}%{}", prefix, p / 100, p % 100, delete_mark),
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

                // 预览区（全屏宽，约 1/3 高）
                RoundedRectangle::new(
                    Rectangle::new(Point::new(preview_x, preview_y), Size::new(vw, preview_h as u32)),
                    corners,
                )
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
                    let pad_x = preview_x + 4;
                    let top = preview_y + 4;
                    let bottom = top + (preview_h as i32) - 8;
                    let clip = Rectangle::new(
                        Point::new(pad_x, top),
                        Size::new(vw - 8, (preview_h - 8) as u32),
                    );
                    let mut clipped_display = display.clipped(&clip);
                    // 手动折行 + render_aligned 逐行绘制（与正文一致的渲染方式）：
                    // TextBox 在 u8g2 字体下行高/基线会让每行顶部被裁。
                    // 宽度按等宽近似（CJK≈15、ASCII≈8），略高估以免右侧贴边被裁。
                    let full_w: i32 = 15;
                    let half_w: i32 = 8;
                    let line_h: i32 = 16;
                    let max_w = (vw as i32) - 8;
                    let mut emit = |s: &str, y: i32| {
                        let _ = font.render_aligned(
                            s,
                            Point::new(pad_x, y),
                            VerticalPosition::Center,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            &mut clipped_display,
                        );
                    };
                    let bytes = self.bookmark_preview.as_bytes();
                    let mut y_center = top + line_h / 2;
                    let mut line_start = 0usize;
                    let mut i = 0usize;
                    let mut w = 0i32;
                    while i < bytes.len() {
                        let b = bytes[i];
                        let is_nl = b == b'\n';
                        let clen = match b {
                            0..=0x7F => 1,
                            0xC0..=0xDF => 2,
                            0xE0..=0xEF => 3,
                            0xF0..=0xF7 => 4,
                            _ => 1,
                        };
                        let clen = clen.min(bytes.len() - i);
                        let cw = if b < 0x80 { half_w } else { full_w };
                        if is_nl || w + cw > max_w {
                            if line_start < i {
                                if let Ok(s) = core::str::from_utf8(&bytes[line_start..i]) {
                                    emit(s, y_center);
                                }
                            }
                            y_center += line_h;
                            line_start = if is_nl { i + clen } else { i };
                            w = if is_nl { 0 } else { cw };
                            if is_nl {
                                i += clen;
                                continue;
                            }
                        } else {
                            w += cw;
                        }
                        i += clen;
                    }
                    if line_start < bytes.len() && y_center - line_h / 2 <= bottom {
                        if let Ok(s) = core::str::from_utf8(&bytes[line_start..]) {
                            emit(s, y_center);
                        }
                    }
                }
            }
            MenuState::Closed => {}
        }
    }

    /// 底部状态行（左：电量图标+时间；右：阅读进度%）+ 进度条。
    fn render_progress(&self, display: &mut crate::display::EpdDisplay, time_str: &str) {
        let total = self.ttf_file_len;
        if total == 0 {
            return;
        }
        // 进度按 ttf_offset（当前页起点）；但 ttf_offset+一页(ttf_end) ≥ file_len 即最后一页 → 算 100%
        let current = if self.ttf_end >= total {
            total
        } else {
            self.ttf_offset.min(total)
        };
        let vw = super::visual_width();
        let vh = super::visual_height();
        let bar_height: u32 = 3;
        let margin: i32 = 2;
        let bar_y = vh as i32 - bar_height as i32 - margin;
        let bar_full_width = vw as i32 - margin * 2;

        // 状态行：进度条上方。左：电量图标（16×9）+ 时间；右：阅读进度百分比
        let icon_h: i32 = 9;
        let status_top = bar_y - icon_h - 2;
        let icon_cy = status_top + icon_h / 2;
        let bat_pct = crate::battery::last_percent();
        let font12: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312>()
            .with_ignore_unknown_chars(true);

        // 电池图标：轮廓 + 右侧电极 + 电量填充（画法同 widgets/battery.rs，无百分比文字）
        let origin = Point::new(margin, status_top);
        let outline = PrimitiveStyleBuilder::new()
            .stroke_color(Black)
            .stroke_width(1)
            .build();
        let fill = PrimitiveStyleBuilder::new().fill_color(Black).build();
        Rectangle::new(origin, Size::new(16, 9))
            .into_styled(outline)
            .draw(display)
            .ok();
        Rectangle::new(Point::new(origin.x + 16, status_top + 2), Size::new(2, 5))
            .into_styled(fill)
            .draw(display)
            .ok();
        let inner_w: u32 = 16u32.saturating_sub(4);
        if bat_pct > 0 {
            let fill_w = (inner_w * bat_pct.min(100)) / 100;
            if fill_w > 0 {
                Rectangle::new(
                    Point::new(origin.x + 2, status_top + 2),
                    Size::new(fill_w, 5),
                )
                .into_styled(fill)
                .draw(display)
                .ok();
            }
        }

        // 时间：图标右侧
        if !time_str.is_empty() {
            let _ = font12.render_aligned(
                time_str,
                Point::new(margin + 16 + 2 + 4, icon_cy),
                VerticalPosition::Center,
                HorizontalAlignment::Left,
                FontColor::Transparent(Black),
                display,
            );
        }

        // 右：阅读进度（两位小数）
        let v = (current as u64 * 10_000 / total as u64) as u32; // 百分比×100
        let mut pct_s: heapless::String<12> = heapless::String::new();
        use core::fmt::Write as _;
        let _ = write!(pct_s, "{}.{:02}%", v / 100, v % 100);
        let _ = font12.render_aligned(
            pct_s.as_str(),
            Point::new(vw as i32 - margin, icon_cy),
            VerticalPosition::Center,
            HorizontalAlignment::Right,
            FontColor::Transparent(Black),
            display,
        );

        // 进度条
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
        let swap_keys = crate::storage::read_reading_swap_keys();
        let mut font_name: String<32> = String::new();
        let fname = settings.font_name_str();
        if is_ttf(fname) || is_bitmap(fname) {
            let _ = font_name.push_str(fname);
        } else {
            let _ = font_name.push_str("font.ttf");
        }

        let mut temp = Self {
            running: false,
            reading: false,
            need_render: false,
            need_text_render: false,
            last_menu_state_id: 0,
            choose_index: 0,
            menus: None,
            log_vec: None,
            menu_state: MenuState::Closed,
            save_bookmark_flag: false,
            delete_bookmark_flag: false,
            need_save_position: false,
            need_load_preview: false,
            need_reload_progress: false,
            bookmark_preview: String::new(),
            rotate,
            swap_keys,
            exit_selected: false,
            jump_accel: 0,
            book_progress: Vec::new(),
            books_dir_ptr: core::ptr::null_mut(),
            font_file_ptr: core::ptr::null_mut(),
            ttf_offset: 0,
            ttf_end: 0,
            ttf_file_len: 0,
            ttf_hist: Vec::new(),
            ttf_at_end: false,
            ttf_last_hint: false,
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

            // 菜单打开时，仅跨状态切换（面板几何不同，避免旧面板残影）或文字确实改变
            // （旋转屏幕 / 排版改字号行距）才清屏 + 重绘正文；同状态内移动光标只重绘
            // 菜单叠层——跳过 SD 读 + TTF 光栅化，菜单操作明显更流畅。
            // 叠层面板自带白色填充，重画时自擦除上一帧叠层内容，不清屏也不残留。
            let menu_open = self.reading && !matches!(self.menu_state, MenuState::Closed);
            let prev_id = self.last_menu_state_id;
            let cur_id = menu_state_id(&self.menu_state);
            // 跨"菜单已打开"的状态切换（如 Popup↔字体列表）：面板几何不同，只局部重绘
            // 叠层会留下旧面板残影，故需全量重绘。从 Closed 刚打开菜单时无旧面板，可只重绘叠层。
            let panel_change = prev_id != 0 && cur_id != 0 && prev_id != cur_id;
            let do_text = !menu_open || panel_change || self.need_text_render;
            if self.need_text_render {
                // 字号/行距/旋转都改变分页边界 → 历史栈里的旧布局页起点全部作废（同换字体清栈）
                self.ttf_hist.clear();
            }
            self.last_menu_state_id = cur_id;
            self.need_text_render = false;

            if !do_text {
                // 仅菜单叠层重绘：缓冲区保留上一帧正文，叠层重画即可。
                if let Some(display) = display_mut() {
                    self.render_menu_overlay(display);
                }
                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                return;
            }

            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(Color::White);
                let vw = super::visual_width();
                let vh = super::visual_height();
                let center = Point::new(vw as i32 / 2, vh as i32 / 2);

                if !self.reading {
                    // 书单列表
                    if let Some(ref menus) = self.menus {
                        // 截断过长书名，避免与右侧进度百分比重叠（窄屏尤甚）
                        // max_w：书名从 x=15 起，右侧留 ~55px 给 "100%" 进度
                        let max_w = vw as i32 - 70;
                        // names 须先于 all_items 声明（all_items 借用它，析构逆序要求被借者后释）
                        let mut names: Vec<alloc::string::String, 20> = Vec::new();
                        let mut all_items: Vec<&str, 20> = Vec::new();
                        let _ = all_items.push("退出");
                        for item in menus.iter() {
                            if names.push(truncate_name(item.as_str(), max_w)).is_err() {
                                break;
                            }
                        }
                        for n in names.iter() {
                            if all_items.push(n.as_str()).is_err() {
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
                    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>();
                    let font = font.with_ignore_unknown_chars(true);

                    let ttf_offset = self.ttf_offset;
                    let ttf_file_len = self.ttf_file_len;
                    let ttf_px = self.ttf_px;
                    let line_gap = self.line_gap;
                    let bd_ptr = self.books_dir_ptr;
                    let font_ptr = self.font_file_ptr;
                    let bm_cell = bm_size_of(self.font_name.as_str());
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
                            let mut n = 0usize;
                            if let Ok(mut bf) =
                                bd.open_long_name_file_in_dir(&file_name, embedded_sdmmc::Mode::ReadOnly)
                            {
                                n = crate::ttf_sd::read_book_chunk(&mut bf, ttf_offset, ws);
                                bf.close();
                            }
                            if n > 0 {
                                let text_area_h = vh as i32
                                    - super::PROGRESS_AREA_HEIGHT as i32 - 2;
                                if let Some(cell) = bm_cell {
                                    // 点阵路径：内置 u8g2 位图，固定单元格宽度（中文满宽/英文半宽）
                                    let bm_font = match cell {
                                        12 => FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312>(),
                                        14 => FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>(),
                                        _ => FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>(),
                                    };
                                    let bm_font = bm_font.with_ignore_unknown_chars(true);
                                    let full_w = cell as i32;
                                    let half_w = (cell / 2) as i32;
                                    let base_h = (cell as i32 + line_gap).max(1);
                                    let max_lines = ((text_area_h / base_h) as u32).max(1);
                                    let line_h = text_area_h / max_lines as i32;
                                    let consumed = super::bitmap::paginate_render_bitmap(
                                        &bm_font, &ws.book_buf, n, display,
                                        Point::new(super::text_left_margin(), 2),
                                        super::text_width() as i32, line_h, max_lines,
                                        full_w, half_w,
                                    );
                                    result = Some(consumed as u32);
                                } else if !font_ptr.is_null() {
                                    let ff: &mut ActualFile = unsafe { &mut *font_ptr };
                                    if let Some(fi) = crate::ttf_sd::open_font(ff, ws) {
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
                    }

                    match result {
                        Some(consumed) => {
                            self.ttf_end = ttf_offset + consumed;
                            // 当前页已是最后一页：下一页起点 >= 文件末尾
                            self.ttf_at_end = ttf_file_len > 0
                                && ttf_offset + consumed >= ttf_file_len;
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

                    // 在最后一页仍按了前进：在当前页上叠一行提示（页内容保留），提示后清标志
                    if self.ttf_last_hint {
                        self.ttf_last_hint = false;
                        let _ = font.render_aligned(
                            "已是最后一页",
                            center,
                            VerticalPosition::Center,
                            HorizontalAlignment::Center,
                            FontColor::Transparent(Black),
                            display,
                        );
                    }
                    // 底部状态行数据：时间（左）+ 电量（右），位于进度条上方
                    let time_str = if let Some(clock) = crate::worldtime::get_clock() {
                        let t = clock.local().await;
                        let mut s: heapless::String<8> = heapless::String::new();
                        use core::fmt::Write as _;
                        let _ = write!(s, "{:02}:{:02}", t.hour(), t.minute());
                        s
                    } else {
                        heapless::String::new()
                    };
                    self.render_progress(display, &time_str);
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

                                // 字体独立放在 fonts/ 目录（不存在则仅用内置点阵字体）
                                let mut fonts_dir_opt: Option<ActualDirectory<'_>> =
                                    root.open_dir("fonts").ok();

                                // 扫描 .ttf 文件进 font_list（长文件名 + 短文件名回退）
                                {
                                    let mut fonts_found: Vec<String<32>, FONT_LIST_CAP> = Vec::new();
                                    if let Some(fonts_dir) = fonts_dir_opt.as_mut() {
                                        let mut storage = [0u8; 512];
                                        let mut lfn_buffer = embedded_sdmmc::LfnBuffer::new(&mut storage);
                                        let _ = fonts_dir.iterate_dir_lfn(&mut lfn_buffer, |dir, lfn| {
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
                                                // 预留末尾 3 个内置点阵位，TTF 超出直接跳过
                                                if name.len() <= 32 && fonts_found.len() < FONT_LIST_CAP - 3 {
                                                    if let Ok(s) = String::<32>::from_str(&name) {
                                                        let _ = fonts_found.push(s);
                                                    }
                                                }
                                            }
                                            ControlFlow::Continue(())
                                        });
                                    }
                                    self.font_list = fonts_found;
                                    // 追加内置点阵字体（u8g2 wqy，无需 SD 文件），大→小
                                    let _ = self.font_list.push(String::from_str("点阵16").unwrap());
                                    let _ = self.font_list.push(String::from_str("点阵14").unwrap());
                                    let _ = self.font_list.push(String::from_str("点阵12").unwrap());
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
                                // 点阵字体用内置 u8g2，无需打开 .ttf
                                if !is_bitmap(fname) {
                                    if let Some(fd) = fonts_dir_opt.as_mut() {
                                        if let Ok(f) = SdMount::open_file_by_name(
                                            fd,
                                            fname,
                                            embedded_sdmmc::Mode::ReadOnly,
                                        ) {
                                            // 句柄实际只依赖 volume_mgr（整个阅读期有效），擦除生命周期为 'static。
                                            let f_static: ActualFile<'static> = unsafe { core::mem::transmute(f) };
                                            ttf_font_file = Some(f_static);
                                        } else {
                                            println!("[read ttf] {} 打开失败", fname);
                                        }
                                    }
                                }
                                self.font_file_ptr = match ttf_font_file.as_mut() {
                                    Some(f) => f as *mut ActualFile<'static>,
                                    None => core::ptr::null_mut(),
                                };

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
                                        let logfile = SdMount::open_log_file(
                                            &mut books_dir,
                                            &book_name,
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
                                        self.need_render = true;
                                    }

                                    // 持久化阅读位置到 .log[0]
                                    // 返回书单：重载每本书的阅读进度（读到最新 .log[0]）
                                    if self.need_reload_progress {
                                        self.need_reload_progress = false;
                                        self.load_book_progress(&mut books_dir);
                                        self.need_render = true;
                                    }

                                    // 换字体：重开句柄 + 清字形缓存 + 清字体表缓存（fi）
                                    if self.need_reload_font {
                                        self.need_reload_font = false;
                                        ttf_font_file.take();
                                        let fname = self.font_name.as_str();
                                        if is_bitmap(fname) {
                                            // 点阵：用内置 u8g2，无 .ttf 句柄
                                            self.font_file_ptr = core::ptr::null_mut();
                                        } else {
                                            // TTF：从 fonts/ 打开（fonts/ 不存在或打开失败都视为失败）
                                            let opened: Option<ActualFile<'_>> = fonts_dir_opt
                                                .as_mut()
                                                .and_then(|fd| {
                                                    SdMount::open_file_by_name(
                                                        fd,
                                                        fname,
                                                        embedded_sdmmc::Mode::ReadOnly,
                                                    )
                                                    .ok()
                                                });
                                            if let Some(f) = opened {
                                                let f_static: ActualFile<'static> =
                                                    unsafe { core::mem::transmute(f) };
                                                ttf_font_file = Some(f_static);
                                                self.font_file_ptr = ttf_font_file.as_mut().unwrap() as *mut ActualFile<'static>;
                                                if let Some(ws) = self.ttf_ws.as_mut() {
                                                    crate::ttf_sd::cache_clear(ws, self.ttf_px);
                                                    ws.fi = None;
                                                }
                                            } else {
                                                println!("[read ttf] 重开字体失败: {}", fname);
                                                self.font_file_ptr = core::ptr::null_mut();
                                            }
                                        }
                                        // 换字体 → 页边界变，历史栈作废（点阵/TTF 通用）
                                        self.ttf_hist.clear();
                                        self.ttf_at_end = false;
                                        self.ttf_end = self.ttf_offset.min(self.ttf_file_len);
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
                                                let logfile = SdMount::open_log_file(
                                                    &mut books_dir,
                                                    &book_name,
                                                    embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                                                );
                                                if let Ok(mut f) = logfile {
                                                    TxtReader::save_log_raw(&mut f, lv);
                                                    f.close();
                                                }
                                                let new_bm_count = if lv.len() > 1 { lv.len() - 1 } else { 0 };
                                                if let MenuState::BookmarkList { ref mut bm_index, .. } = self.menu_state {
                                                    if *bm_index as usize >= new_bm_count && *bm_index > 0 {
                                                        *bm_index -= 1;
                                                    }
                                                }
                                            }
                                        }
                                        // 删除后重载当前选中项预览（交由下方 need_load_preview 块读取）
                                        self.need_load_preview = true;
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
                                                    // 截到 UTF-8 字符边界：512 字节中文几乎必然在
                                                    // 多字节字符中间截断（512%3=2），from_utf8 全有或全无，
                                                    // 不截就会整段失败 → 预览空白。
                                                    let mut start = 0usize;
                                                    while start < got && (buf[start] & 0xC0) == 0x80 {
                                                        start += 1; // 跳过开头可能的续字节碎片
                                                    }
                                                    let mut end = got;
                                                    while end > start {
                                                        let b = buf[end - 1];
                                                        if (b & 0xC0) == 0x80 {
                                                            end -= 1; // 末尾续字节
                                                            continue;
                                                        }
                                                        if (b & 0x80) != 0 {
                                                            end -= 1; // 不完整的起始字节
                                                        }
                                                        break;
                                                    }
                                                    if let Ok(s) = core::str::from_utf8(&buf[start..end]) {
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

                                    // 持久化阅读位置到 .log[0]（render 之后 ttf_at_end 新鲜）：
                                    // 低 31 位 = ttf_offset（页起点，续读回到本页）；
                                    // bit31 = 是否已到末页（书单据此显示 100%，翻页时更新）
                                    if self.need_save_position {
                                        self.need_save_position = false;
                                        if self.ttf_file_len > 0 {
                                            let pos = self.ttf_offset | (if self.ttf_at_end { 1u32 << 31 } else { 0 });
                                            let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
                                            let logfile = SdMount::open_log_file(
                                                &mut books_dir,
                                                &book_name,
                                                embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                                            );
                                            if let Ok(mut f) = logfile {
                                                if self.log_vec.is_none() {
                                                    self.log_vec = Some(Vec::new());
                                                }
                                                if let Some(ref mut lv) = self.log_vec {
                                                    TxtReader::save_log(&mut f, lv, pos, false);
                                                }
                                                f.close();
                                            }
                                        }
                                    }

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
                                        if let Ok(mut f) = SdMount::open_log_file(
                                            &mut books_dir,
                                            &book_name,
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
                    | MenuState::FontList { .. }
                    | MenuState::BookmarkList { .. } => {
                        // 子菜单内长按 3：统一退回 Popup（取消当前操作）
                        mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                        mut_ref.bookmark_preview.clear();
                        mut_ref.need_render = true;
                    }
                    MenuState::Layout { .. } => {
                        // 排版长按 3：保存设置后关闭（与"返回"一致）
                        mut_ref.save_reading_settings();
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::Popup { .. } => {
                        // 主菜单打开时长按 3：关闭菜单，继续阅读（而非返回书单）
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    _ => {
                        // 菜单未打开（Closed）时长按 3：返回书单
                        if mut_ref.reading {
                            mut_ref.reading = false;
                            unsafe {
                                core::ptr::addr_of_mut!(PAGE_INDEX).write(None);
                            }
                            mut_ref.need_reload_progress = true;
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
                                mut_ref.need_reload_progress = true;
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
                                mut_ref.need_load_preview = true;
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
                                mut_ref.menu_state = MenuState::Layout { cursor: 0, editing: false };
                                mut_ref.need_render = true;
                                return;
                            }
                            6 => {
                                mut_ref.menu_state = MenuState::FontList { font_index: 0 };
                                mut_ref.need_render = true;
                                return;
                            }
                            7 => {
                                // 旋转屏幕：保持菜单打开，便于连续旋转（再按 Key3 继续转，
                                // 移走光标或选“取消”关闭菜单）
                                mut_ref.rotate = (mut_ref.rotate + 1) % 4;
                                super::set_rotation_state(mut_ref.rotate);
                                if let Some(display) = display_mut() {
                                    display.set_rotation(super::current_rotation());
                                }
                                mut_ref.save_reading_settings();
                                mut_ref.need_text_render = true;
                                mut_ref.need_render = true;
                                return;
                            }
                            8 => {
                                crate::sleep::refresh_active_time().await;
                                crate::sleep::to_sleep_tips(Duration::from_secs(0), Duration::from_secs(0), true).await;
                                return;
                            }
                            9 => {
                                // 方向键互换：切换开关，保持菜单打开便于确认状态
                                mut_ref.swap_keys = !mut_ref.swap_keys;
                                crate::storage::write_reading_swap_keys(mut_ref.swap_keys);
                                mut_ref.need_render = true;
                                return;
                            }
                            _ => {} // 10 = 取消
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
                    MenuState::Layout { ref mut cursor, ref mut editing } => {
                        if *editing {
                            // 退出编辑模式
                            *editing = false;
                            // 字号改了需清缓存
                            if *cursor == 0 {
                                if let Some(ws) = mut_ref.ttf_ws.as_mut() {
                                    crate::ttf_sd::cache_clear(ws, mut_ref.ttf_px);
                                }
                            }
                        } else {
                            match *cursor {
                                0 | 1 => { *editing = true; }
                                2 => {
                                    // 返回
                                    mut_ref.save_reading_settings();
                                    mut_ref.menu_state = MenuState::Closed;
                                }
                                _ => {}
                            }
                        }
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

        // Key1 long hold: 向下滚动 / 加速跳转 / 下一项（“方向键互换”开 → 向上）
        event::on_target(EventType::KeyLongIng(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                let down = !mut_ref.swap_keys;
                mut_ref.dir_key_long(down).await;
            });
        })
        .await;

        // Key2 long hold: 向上滚动 / 加速跳转 / 上一项（“方向键互换”开 → 向下）
        event::on_target(EventType::KeyLongIng(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                let down = mut_ref.swap_keys;
                mut_ref.dir_key_long(down).await;
            });
        })
        .await;

        // Key1 short: 向下 / +1 / 下一页 / 下一本（“方向键互换”开 → 向上）
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                let down = !mut_ref.swap_keys;
                mut_ref.dir_key_short(down).await;
            });
        })
        .await;

        // Key2 short: 向上 / -1 / 上一页 / 上一本（“方向键互换”开 → 向下）
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                let down = mut_ref.swap_keys;
                mut_ref.dir_key_short(down).await;
            });
        })
        .await;
    }
}
