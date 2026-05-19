use alloc::boxed::Box;
use heapless::String;
use heapless::Vec;
use core::str::FromStr;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration,  Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::prelude::Point;
use esp_println::println;
use esp_hal::ram;
use epd_waveshare::color::{Black, Color, White};

use epd_waveshare::prelude::Display;
use crate::{ event};
use crate::display::{display_mut,  RENDER_CHANNEL, RenderInfo};
use crate::event::EventType;
use crate::pages::{MenuItem, Page, PageEnum};
use crate::pages::calendar::CalendarPage;
use crate::pages::PageEnum::{ECalendarPage,  EClockPage, EDebugPage, EReadPage, ESettingPage, ETimerPage, EWeatherPage, EImageListPage};
use crate::pages::IconType;
use crate::widgets::icon_grid_widget::IconGridWidget;
use crate::pages::debug_page::DebugPage;
use crate::pages::read::ReadPage;
use crate::pages::image_page::ImagePage;
use crate::pages::setting_page::SettingPage;
use crate::pages::weather::WeatherPage;
use crate::sleep::to_sleep_tips;
use crate::storage::NvsStorage;

static MAIN_PAGE:Mutex<CriticalSectionRawMutex,Option<MainPage> > = Mutex::new(None);

#[ram(unstable(rtc_fast))]
static mut PAGE_INDEX:i32 = 1;

///每个page 包含状态与绘制与逻辑处理
///状态通过事件改变，并触发绘制
pub struct MainPage{
    current_page:Option<u32>,
    choose_index:u32,
    is_long_start:bool,
    need_render:bool,
    menus:Option<Vec<MenuItem,20>>
}

impl MainPage {

    pub async fn init(_spawner: Spawner){
        let mut page_index = unsafe { *core::ptr::addr_of!(PAGE_INDEX) };
        
        
        // 检查是否有错误日志，如果有则进入debug_page
        #[cfg(feature = "enable_debug")]
        if let Ok(error_log) = crate::storage::ErrorLogStorage::read() {
            if error_log.error_count > 0 {
                println!("=== 检测到错误日志，进入调试模式 ===");
                println!("错误计数: {}", error_log.error_count);
                println!("最新错误信息:");
                println!("{}", error_log.last_error);
                println!("================================");
                page_index = 5;
            }
        } 
        MAIN_PAGE.lock().await.replace(MainPage::new());

        Self::bind_event(MAIN_PAGE.lock().await.as_mut().unwrap()).await;

        if page_index > -1 {
            MAIN_PAGE.lock().await.as_mut().unwrap().current_page = Some(page_index as u32);
        }else{
            MAIN_PAGE.lock().await.as_mut().unwrap().current_page = None;
        }
    }

    pub async fn get_mut() -> Option<&'static mut MainPage> {
        unsafe {
            let ptr: *mut MainPage =  MAIN_PAGE.lock().await.as_mut().unwrap()  as *mut MainPage;
            Some(&mut *ptr)
        }
    }


    fn increase(&mut self){
        if self.choose_index < (self.menus.as_mut().unwrap().len() - 1) as u32 {
            self.choose_index += 1;
            self.need_render = true;
        }
    }

    fn decrease(&mut self){
        if self.choose_index > 0 {
            self.choose_index -= 1;
            self.need_render = true;
        }
    }

    async fn back(&mut self){
        self.current_page = None;
        self.need_render = true;
        Self::bind_event(self).await;
    }
}
impl Page for  MainPage{

    fn new()->Self{

        let mut menus = Vec::new();

        menus.push(MenuItem::new(String::<20>::from_str("电子书").unwrap(), EReadPage, IconType::Book));
        menus.push(MenuItem::new(String::<20>::from_str("天气").unwrap(), EWeatherPage, IconType::Weather));
        menus.push(MenuItem::new(String::<20>::from_str("日历").unwrap(), ECalendarPage, IconType::Calendar));
        menus.push(MenuItem::new(String::<20>::from_str("图片").unwrap(), EImageListPage, IconType::Image));
        menus.push(MenuItem::new(String::<20>::from_str("设置").unwrap(), ESettingPage, IconType::Settings));
        menus.push(MenuItem::new(String::<20>::from_str("调试").unwrap(), EDebugPage, IconType::Debug));

        Self{
            current_page:None,
            choose_index:0,
            is_long_start:false,
            need_render:true,
            menus:Some(menus)
        }
    }
    async fn bind_event(&mut self){
        event::clear().await;
        event::on(EventType::KeyShort(2),  move |_info|  {
            println!("current_page:" );
            return Box::pin(async {
                Self::get_mut().await.unwrap().increase();
                println!("current_page:{}",Self::get_mut().await.unwrap().choose_index );
            });
        }).await;

        event::on(EventType::KeyLongEnd(1),  |_info|  {
            println!("current_page:" );
            return Box::pin( async {

            });
        }).await;

        event::on(EventType::KeyLongEnd(2),  |_info|  {
            println!("current_page:" );
            return Box::pin( async {

            });
        }).await;
        event::on(EventType::KeyShort(1),  |_info|  {
            println!("current_page:" );
            return Box::pin( async {
                Self::get_mut().await.unwrap().decrease();
                println!("current_page:{}",Self::get_mut().await.unwrap().choose_index );
            });
        }).await;
        event::on(EventType::KeyShort(3),  |_info|  {
            println!("current_page:" );
            return Box::pin( async {
                let mut_ref = Self::get_mut().await.unwrap();
                mut_ref.current_page = Some( mut_ref.choose_index);
                unsafe {
                    unsafe { *core::ptr::addr_of_mut!(PAGE_INDEX) = mut_ref.choose_index as i32; }
                }
                println!("current_page:{}",Self::get_mut().await.unwrap().choose_index );
            });
        }).await;

    }



    //通过具体的状态绘制
    async fn render(&mut self) {
        if self.need_render {

            if let Some(display) = display_mut() {
                self.need_render = false;

                let _ = display.clear_buffer(Color::White);

                let grid_items: Vec<(IconType, &str), 20> = self.menus.as_ref().unwrap()
                    .iter()
                    .map(|v| (v.icon, v.title.as_str()))
                    .collect();

                let columns = 3;
                let mut grid_widget = IconGridWidget::new(
                    Point::new(0, 0),
                    Black,
                    White,
                    display.bounding_box().size,
                    columns,
                    grid_items,
                );
                grid_widget.choose(self.choose_index as usize);
                let _ = grid_widget.draw(display);

                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                println!("has display:{}", self.choose_index);


            } else {
                println!("no display");
            }
        }

    }

    async fn run(&mut self,spawner: Spawner){

        // crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
        // RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
        // 
        // crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
        loop {

            if  None == self.current_page {
                self.render().await;
                to_sleep_tips(Duration::from_secs(0), Duration::from_secs(30),true).await;
                Timer::after(Duration::from_millis(50)).await;
                continue;
            }
            let current_page = self.current_page.unwrap();

            let menu_item = &self.menus.as_mut().unwrap()[current_page as usize];
            match menu_item.page_enum {
                PageEnum::EMainPage => {

                }
                EReadPage => {
                    let mut read_page = ReadPage::new();
                    read_page.bind_event().await;
                    read_page.run(spawner).await;
                    self.back().await;
                }
                EImageListPage => {
                    let mut image_page = ImagePage::new();
                    image_page.bind_event().await;
                    image_page.run(spawner).await;
                    self.back().await;
                }
                EClockPage => {
                    self.back().await;
                }
                ETimerPage => {
                    self.back().await;
                }
                EWeatherPage => {
                    let mut weather_page = WeatherPage::new();
                    weather_page.bind_event().await;
                    weather_page.run(spawner).await;
                    self.back().await;
                }
                ECalendarPage => {
                    let mut calendar_page = CalendarPage::new();
                    calendar_page.bind_event().await;
                    calendar_page.run(spawner).await;
                    self.back().await;
                }
                ESettingPage =>{
                    let mut qrcode_page = SettingPage::new();
                    qrcode_page.bind_event().await;
                    qrcode_page.run(spawner).await;
                    self.back().await;
                }
                EDebugPage =>{
                    let mut debug_page = DebugPage::new();
                    debug_page.bind_event().await;
                    debug_page.run(spawner).await;
                    self.back().await;
                }
                _ => { self.back().await;}
            }
            to_sleep_tips(Duration::from_secs(0), Duration::from_secs(30),true).await;
            Timer::after(Duration::from_millis(50)).await;
        }
    }

}

