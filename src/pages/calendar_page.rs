use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use core::future::Future;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::ascii::FONT_6X9;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Point, Size};
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Baseline, Text, TextStyle, TextStyleBuilder};
use esp_println::println;
use epd_waveshare::color::{Black, Color, White};
use crate::pages::Page;
use time::{Date, OffsetDateTime, Month};
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::fonts;
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::sleep::{refresh_active_time, to_sleep};
use crate::widgets::calendar::Calendar;

use crate::worldtime::{ get_clock, sync_time_success};

pub struct CalendarPage {
    running:bool,
    need_render:bool,
}

impl CalendarPage {

    async fn back(&mut self){
        self.running = false;
    }
}

impl Page for CalendarPage {
     fn new() -> Self {
        Self{
            running: false,
            need_render: false,

        }
    }

    async fn render(&mut self)  {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                let _ = display.clear(White);

                if sync_time_success() {
                    if let Some(clock) = get_clock() {

                        let local = clock.local().await;
                        let year = local.year();
                        let month = local.month();
                        let today = local.date();
                        let mut calendar = Calendar::new(Point::default(), Size::default(), year, month, today, Black, White);
                        calendar.position = Point::new(0,0);
                        calendar.size = Size::new(display.size().width ,display.size().height);
                        calendar.draw(display);



                    }
                }else{
                    let style =
                        U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, Black);
                    let _ = Text::new("同步时间", Point::new(0,50), style.clone()).draw(display);
                }

            }

            RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;
        }
    }

    async fn run(&mut self, spawner: Spawner) {
        self.running = true;
        refresh_active_time().await;
        loop {
            if !self.running {
                break;
            }

            self.need_render = true;
            self.render().await;

            if sync_time_success() {
                to_sleep(Duration::from_secs(3600), Duration::from_secs(10)).await;
            }
            Timer::after(Duration::from_millis(50)).await;
        }
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        event::on_target(EventType::KeyShort(5),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.back().await;
            });
        }).await;


        event::on_target(EventType::KeyShort(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                refresh_active_time().await;
            });
        }).await;
    }
}

