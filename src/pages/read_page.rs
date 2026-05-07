use alloc::boxed::Box;
use alloc::format;
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

const MENU_ITEMS: &[&str] = &["返回书单", "收藏书签", "打开书签", "跳转页码", "旋转屏幕", "重建索引"];

enum MenuState {
    Closed,
    Popup { menu_index: u32 },
    JumpInput { input_num: u32 },
    BookmarkList { bm_index: u32 },
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
    /// 0=Rotate90, 1=Rotate270 (upside-down portrait, same page indexing)
    flipped:bool,
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

                font.render_aligned(
                    format_args!("{}", input_num),
                    Point::new(center_x, jump_y + 48),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                ).ok();

                font.render_aligned(
                    "1+ 2- 3确认",
                    Point::new(center_x, jump_y + 72),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                ).ok();
            }
            MenuState::BookmarkList { bm_index } => {
                // Show bookmark list from log_vec[1..] (index 0 is last read position)
                let bookmarks: Vec<u32, LOG_VEC_MAX> = self.log_vec.as_ref()
                    .map(|lv| lv.iter().skip(1).copied().collect())
                    .unwrap_or_default();

                let bm_count = bookmarks.len() as u32;
                let list_height = if bm_count > 0 { bm_count * menu_item_height + menu_padding * 2 } else { 50 };
                let list_x = ((vw - menu_width) / 2) as i32;
                let list_y = ((vh - list_height) / 2) as i32;

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

                if bm_count == 0 {
                    font.render_aligned(
                        "暂无书签",
                        Point::new((vw / 2) as i32, list_y + 25),
                        VerticalPosition::Center,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    ).ok();
                } else {
                    for (i, &page) in bookmarks.iter().enumerate() {
                        let item_y = list_y + menu_padding as i32 + (i as u32 * menu_item_height) as i32;
                        let is_selected = i as u32 == bm_index;

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
                        font.render_aligned(
                            format_args!("{}书签 第{}页", prefix, page),
                            Point::new(list_x + menu_padding as i32, item_y + menu_item_height as i32 / 2),
                            VerticalPosition::Center,
                            HorizontalAlignment::Left,
                            text_color,
                            display,
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
            flipped: false,
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
                            let books_dir_res = root.open_dir("books");
                            if let Ok(mut books_dir) = books_dir_res {
                                let books = SdMount::get_books(&mut books_dir).unwrap();
                                self.menus = Some(books);



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
                if mut_ref.reading {
                    if matches!(mut_ref.menu_state, MenuState::Closed) {
                        // Open menu
                        mut_ref.menu_state = MenuState::Popup { menu_index: 0 };
                        mut_ref.need_render = true;
                    } else {
                        // Close menu
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                } else {
                    mut_ref.back().await;
                }
            });
        }).await;

        // Key3 short: select menu item / confirm jump / toggle reading mode
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
                                // 打开书签 — 进入书签列表
                                mut_ref.menu_state = MenuState::BookmarkList { bm_index: 0 };
                                mut_ref.need_render = true;
                                return;
                            }
                            3 => {
                                // 跳转页码 — 进入数字输入
                                mut_ref.menu_state = MenuState::JumpInput { input_num: mut_ref.page_index };
                                mut_ref.need_render = true;
                                return;
                            }
                            4 => {
                                // 旋转屏幕 — flip upside down (Rotate90 ↔ Rotate270)
                                mut_ref.flipped = !mut_ref.flipped;
                                if let Some(display) = display_mut() {
                                    display.set_rotation(mut_ref.current_rotation());
                                }
                            }
                            5 => {
                                // 重建索引
                                mut_ref.force_indexing = true;
                                mut_ref.book_pages = None;
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
                    MenuState::BookmarkList { bm_index } => {
                        // 选择书签，跳转到对应页
                        if let Some(ref lv) = mut_ref.log_vec {
                            let bookmarks: Vec<u32, LOG_VEC_MAX> = lv.iter().skip(1).copied().collect();
                            if (bm_index as usize) < bookmarks.len() {
                                mut_ref.do_change_page(bookmarks[bm_index as usize]).await;
                            }
                        }
                        mut_ref.menu_state = MenuState::Closed;
                        mut_ref.need_render = true;
                    }
                    MenuState::Closed => {
                        if mut_ref.reading {
                            mut_ref.reading = false;
                            unsafe { PAGE_INDEX = None; }
                        } else {
                            mut_ref.reading = true;
                            mut_ref.change_page = true;
                            mut_ref.page_index = 0;
                            mut_ref.book_pages = None;
                            unsafe { PAGE_INDEX = Some(mut_ref.choose_index); }
                        }
                        mut_ref.need_render = true;
                    }
                }
            });
        }).await;

        // Key1 long hold: continuous scroll down (book list or menu)
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
                    MenuState::Closed => {
                        if !mut_ref.reading {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 && mut_ref.choose_index < (max - 1) as u32 {
                                mut_ref.choose_index += 1;
                                mut_ref.need_render = true;
                                Timer::after_millis(200).await;
                            }
                        }
                    }
                    _ => {}
                }
            });
        }).await;

        // Key2 long hold: continuous scroll up (book list or menu)
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
                    MenuState::Closed => {
                        if !mut_ref.reading {
                            let max = mut_ref.menus.as_ref().map(|m| m.len()).unwrap_or(0);
                            if max > 0 && mut_ref.choose_index > 0 {
                                mut_ref.choose_index -= 1;
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
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index } => {
                        let bm_count = mut_ref.log_vec.as_ref().map(|lv| if lv.len() > 0 { lv.len() - 1 } else { 0 }).unwrap_or(0);
                        if (*bm_index as usize) < bm_count.saturating_sub(1) {
                            *bm_index += 1;
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
                            mut_ref.need_render = true;
                        }
                    }
                    MenuState::BookmarkList { ref mut bm_index } => {
                        if *bm_index > 0 {
                            *bm_index -= 1;
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