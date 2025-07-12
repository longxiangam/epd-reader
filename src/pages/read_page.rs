use alloc::boxed::Box;
use alloc::format;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{Dimensions, Point, Size};
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
use crate::sd_mount::{ActualDirectory, SD_MOUNT, SdMount};
use crate::sleep::{to_sleep, to_sleep_tips};
use crate::widgets::list_widget::ListWidget;

const PAGES_VEC_MAX:usize = epd2in9_txt::PAGES_VEC_MAX;
const LOG_VEC_MAX:usize = epd2in9_txt::LOG_VEC_MAX;
const ONE_PAGE_CONTENT_LEN:usize = epd2in9_txt::ONE_PAGE_CONTENT_LEN;
pub struct ReadPage{
    running:bool,
    reading:bool,
    need_render:bool,
    change_page:bool,

    force_indexing:bool,
    indexing:bool,
    indexing_process:f32,

    choose_index:u32,
    open_file_name:String<50>,
    menus:Option<Vec<String<50>,40>>,
    book_pages:Option<BookPages>,
    log_vec:Option<Vec<u32,LOG_VEC_MAX>>,
    page_index:u32,
    page_content:String<ONE_PAGE_CONTENT_LEN>,
}

impl ReadPage{
    async fn back(&mut self){
        self.running = false;
    }
    async fn get_page_vec<>(&mut self, books_dir:&mut ActualDirectory<'_>)  {
        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();

        let file_name = format!("{}.txt", book_name);
        let index_name = format!("{}.idx", book_name);
        let mut need_index = false;
        let mut file_len = 0;
        let mut book_pages = None;
        {
            let mut my_file = SdMount::open_file_by_name(books_dir,file_name.as_str(), embedded_sdmmc::Mode::ReadOnly).unwrap();
            file_len = my_file.length();
            my_file.close();
        }

        println!("file len:{}", file_len);
        {
            let mut my_file_index = SdMount::open_file_by_name(books_dir,index_name.as_str(), embedded_sdmmc::Mode::ReadOnly);
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
            book_pages = TxtReader::generate_pages(books_dir,book_name.as_str(), |process|  {
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
        let log_name = format!("{}.log", book_name);
        {
            //读日志
            let mut my_file =  SdMount::open_file_by_name(books_dir,log_name.as_str(), embedded_sdmmc::Mode::ReadOnly);
            if let Ok(mut f) = my_file {
                self.log_vec = Some(TxtReader::read_log(&mut f));
                if let Some(ref lv) = self.log_vec{
                    if lv.len() > 0 {
                        self.page_index = lv[0];
                    }
                }
                f.close();
            }
        }

    }
    async fn get_page_content(&mut self,books_dir:&mut ActualDirectory<'_>){

        let book_name = self.menus.as_ref().unwrap()[self.choose_index as usize].clone();
        //读sd卡目录

        let begin_secs = Instant::now().as_secs();
        println!("begin_time:{}", begin_secs);

        let file_name = format!("{}.txt", book_name);
        let log_name = format!("{}.log", book_name);
        let index_name = format!("{}.idx", book_name);
        if let Some( bp) = self.book_pages.as_mut() {
            let mut begin =  0;
            let mut end =  0;
            {
                let mut index_file = SdMount::open_file_by_name(books_dir,index_name.as_str(), embedded_sdmmc::Mode::ReadOnly);
                if let Ok(mut index_file) = index_file {
                    (begin, end) = bp.get_page_content_position(&mut index_file);
                }
            }
            {
                //let mut my_file =books_dir.open_file_in_dir(file_name.as_str(), embedded_sdmmc::Mode::ReadOnly).unwrap();
                let mut my_file =  SdMount::open_file_by_name(books_dir,file_name.as_str(), embedded_sdmmc::Mode::ReadOnly);
                if let Ok(mut my_file) = my_file {
                    self.page_content = TxtReader::get_page_content(&mut my_file, begin, end);
                    my_file.close();
                }
            }
            {
                let logfile = SdMount::open_file_by_name(books_dir,log_name.as_str(), embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);
                if let Ok(mut f) = logfile {
                   epd2in9_txt::TxtReader::save_log(&mut f,self.log_vec.as_mut().unwrap(), self.page_index as u32, false);
                    f.close();
                }
            }
        }

    }

    async fn do_change_page(&mut self,page_index:u32){


        if page_index >= self.book_pages.as_ref().unwrap().total_page {
            self.page_index = self.book_pages.as_ref().unwrap().total_page;
        }else{
            self.page_index = page_index;
        }
        self.change_page = true;
        self.need_render = true;

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
                if self.indexing {
                    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                    let mut font = font.with_ignore_unknown_chars(true);
                    let _ = font.render_aligned(
                        format_args!("正在创建索引，\n 已创建索引进度：{:.2}%",self.indexing_process),
                        Point::new(display.bounding_box().center().y, display.bounding_box().center().x),
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
                                                                  , Size::new(display.bounding_box().size.height, display.bounding_box().size.width)
                                                                  , menus
                            );
                            list_widget.choose(self.choose_index as usize);
                            let _ = list_widget.draw(display);
                        }
                    } else {
                        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                        let mut font = font.with_ignore_unknown_chars(true);
                        //显示选择书本对应页的内容
                        display.clear_buffer(Color::White);
                        {
                            if self.page_index == self.book_pages.as_ref().unwrap().total_page {
                                let _ = font.render_aligned(
                                    "已是最后一页",
                                    Point::new(display.bounding_box().center().y, display.bounding_box().center().x),
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
                    }
                }
            }


            RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;
        }
    }
    
    

    async fn run(&mut self, spawner: Spawner) {
        if let Some(display) = display_mut() {
           display.set_rotation(DisplayRotation::Rotate90);
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
                                    
                                    self.render().await;


                                    to_sleep_tips(Duration::from_secs(0), Duration::from_secs(120),true).await;
                                    
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
        event::on_target(EventType::KeyLongEnd(3),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.back().await;
            });
        }).await;
        event::on_target(EventType::KeyShort(3),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.reading {
                    mut_ref.reading = false;
                    unsafe {
                        PAGE_INDEX = None;
                    }
                }else{
                    mut_ref.reading = true;
                    mut_ref.change_page = true;
                    mut_ref.page_index = 0;
                    mut_ref.book_pages = None;

                    unsafe {
                        PAGE_INDEX = Some(mut_ref.choose_index);
                    }
                }
                mut_ref.need_render = true;

            });
        }).await;
        event::on_target(EventType::KeyLongEnd(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();

                mut_ref.force_indexing = true;
                mut_ref.book_pages = None;
                println!("显示弹出框");

            });
        }).await;
        event::on_target(EventType::KeyShort(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.reading {
                    mut_ref.do_change_page(mut_ref.page_index + 1).await;
                }else {
                    if mut_ref.choose_index < mut_ref.menus.as_ref().unwrap().len() as u32 {
                        mut_ref.choose_index += 1;
                        mut_ref.need_render = true;
                    }
                }
            });
        }).await;
        event::on_target(EventType::KeyShort(2),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.reading {
                    if mut_ref.page_index > 0 {
                        mut_ref.do_change_page(mut_ref.page_index - 1).await;
                    }
                    println!("page_index {}",mut_ref.page_index);
                }else {
                    if mut_ref.choose_index > 0 {
                        mut_ref.choose_index -= 1;
                        mut_ref.need_render = true;
                    }
                }
            });
        }).await;
    }
}