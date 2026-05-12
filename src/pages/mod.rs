use heapless::String;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use crate::pages::main_page::MainPage;

pub mod main_page;

mod image_page;
mod calendar_page;
pub(crate) mod read_page;
mod read_menu_page;
mod weather_page;
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