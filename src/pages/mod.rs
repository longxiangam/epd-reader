use heapless::String;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use crate::pages::main_page::MainPage;

pub mod main_page;

mod image_page;
mod calendar;
pub mod read;
mod read_menu_page;
mod weather;
mod stock;
pub mod setting_page;
pub mod debug_page;

#[derive(Clone, Copy)]
pub enum IconType {
    Book,
    Image,
    Weather,
    Calendar,
    Settings,
    Debug,
    Stock,
}

enum PageEnum {
    EMainPage,
    EClockPage,
    ETimerPage,
    EWeatherPage,
    ECalendarPage,
    EChip8Page,
    ESettingPage,
    EReadPage,
    EImageListPage,
    EDebugPage,
    EStockPage,
}
struct  MenuItem{
    page_enum:PageEnum,
    title:String<20>,
    icon:IconType,
}
impl MenuItem{
    fn new(title:String<20>, page_enum: PageEnum, icon:IconType) -> MenuItem {
        Self{
            page_enum,
            title,
            icon,
        }
    }
}


pub trait Page {
    fn new() ->Self;
    async fn render(&mut self);

    async fn  run(&mut self,spawner: Spawner);
    async fn bind_event(&mut self);

    fn mut_by_ptr<'a,T>(ptr:Option<usize>)->Option<&'a mut T>{
        unsafe {
            if let Some(v) =  ptr {
                return Some(&mut *(v as *mut T));
            }else{
                return None;
            }
        }
    }

    fn mut_to_ptr<T>(ref_mut:&mut T)->usize{
          ref_mut as *mut T as usize
    }
}



#[embassy_executor::task]
pub async fn main_task(spawner:Spawner){

    MainPage::init(spawner).await;
    loop {

        MainPage::get_mut().await.unwrap().run(spawner).await;

        Timer::after(Duration::from_millis(50)).await;
    }
}

/// 阅读模式入口（重启分模式）：阅读模式启动时由 main 直接 spawn，独占堆做 TTF。
/// ReadPage 退出（running=false）即离开阅读 → 清模式标志 → reboot_sleep 回正常模式。
#[embassy_executor::task]
pub async fn reading_task(spawner: Spawner) {
    // 等显示任务就绪：reading_task 启动比显示任务初始化(EPD init)快，若不等，
    // ReadPage::run 开头的 set_rotation 与首帧 render 会因 display_mut()=None 被跳过
    // → 进阅读首帧空白、且未旋转（点键后显示任务已就绪才正常）。
    {
        let mut waited = 0u32;
        while crate::display::display_mut().is_none() {
            if waited > 150 { break; } // 最多等 ~3s（防御；正常 ~百 ms 就绪）
            waited += 1;
            Timer::after(Duration::from_millis(20)).await;
        }
    }
    let mut p = read::ReadPage::new();
    p.bind_event().await;
    p.run(spawner).await;
    // 离开阅读：清阅读标志 + 主网格菜单位设 -1（显示网格，避免唤醒后又自动进阅读）
    // → 短深睡唤醒回正常模式（rtc_fast 保留，但 READING_MODE=false → 走正常路径）。
    unsafe {
        core::ptr::addr_of_mut!(read::READING_MODE).write(false);
        core::ptr::addr_of_mut!(main_page::PAGE_INDEX).write(-1);
    }
    crate::sleep::reboot_sleep().await;
}