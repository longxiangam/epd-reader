use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use core::future::Future;
use eg_seven_segment::SevenSegmentStyleBuilder;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, OriginDimensions};
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use embedded_layout::align::Align;
use embedded_layout::layout::linear::Horizontal;
use epd_waveshare::color::{Black, Color};
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use esp_println::println;

use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::model::seniverse::{DailyResult, form_json};
use crate::pages::Page;
use crate::request::RequestClient;
use crate::weather::{get_weather, WEATHER_SYNC_SUCCESS};
use crate::widgets::calendar::Calendar;
use crate::wifi::{finish_wifi, use_wifi, WIFI_STATE};
use crate::worldtime::{get_clock, sync_time_success};


pub struct WeatherPage{
    weather_data: Option<DailyResult>,
    running:bool,
    need_render:bool,
    loading:bool,
    error:Option<String>,
}


impl WeatherPage{
    fn draw_clock<D>(display: &mut D, time: &str) -> Result<(), D::Error>
        where
            D: DrawTarget<Color = BinaryColor>,
    {
        let character_style = SevenSegmentStyleBuilder::new()
            .digit_size(Size::new(18, 43))
            .segment_width(4)
            .segment_color(Black)
            .build();

        let text_style = TextStyleBuilder::new()
            .alignment(Alignment::Left)
            .baseline(Baseline::Top)
            .build();

        Text::with_text_style(
            &time,
            Point::new(0, (display.bounding_box().size.height - 45) as i32),
            character_style,
            text_style,
        )
            .draw(display)?;


        Ok(())
    }

}

impl Page for  WeatherPage{
    fn new() -> Self {
        Self{
            weather_data: None,
            running: false,
            need_render: false,
            loading: false,
            error: None,
        }
    }

    async fn render(&mut self)  {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(White);

                let style =
                    U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);



                let mut wifi_finish = false;
                if let Some(crate::wifi::WifiNetState::WifiConnecting) = *WIFI_STATE.lock().await {
                    let _ = Text::new("正在连接网络...", Point::new(0,20), style.clone())
                        .draw(display);
                }else{
                    wifi_finish = true;
                }

                if *WEATHER_SYNC_SUCCESS.lock().await {
                    if let Some(weather) = get_weather() {

                        if let Some(weather) = weather.daily_result.lock().await.as_mut() {

                            let x = 100;
                            let mut y = display.size().height - 45;
                            for one in weather.daily.iter() {
                                let (year, date) = one.date.split_once('-').unwrap();
                                let date = date.replace("-",".");
                                let str = format_args!("{} {}/{},{}/{}℃,湿:{}%,风:{}"
                                                       ,date,one.text_day,one.text_night,one.low,one.high,one.humidity,one.wind_scale).to_string();
                                let _ = Text::new(str.as_str(), Point::new(x, y as i32), style.clone()).draw(display);
                                y+=20;
                            }

                        }
                    }

                }else if(wifi_finish){
                    let _ = Text::new("正在同步天气...", Point::new(0,40), style.clone())
                        .draw(display);
                }
                if sync_time_success() {
                    if let Some(clock) = get_clock() {
                        let local = clock.local().await;
                        let hour = local.hour();
                        let minute = local.minute();
                        let second = local.second();


                        let str = format_args!("{:02}:{:02}",hour,minute).to_string();

                        let date = clock.get_date_str().await;
                        let week = clock.get_week_day().await;

                        Self::draw_clock(display,str.as_str());
                        //因为进行了旋转，这里宽高互换
                        let height = display.size().height;
                        let width = display.size().width;

                
                        let calendar_rect = Rectangle::new(Point::new( 0, 0)
                                                       , Size::new(width,height - 45));
                 


                        let local = clock.local().await;
                        let year = local.year();
                        let month = local.month();
                        let today = local.date();
                        let mut calendar = Calendar::new(Point::default(), Size::default(), year, month, today, Black, epd_waveshare::color::White);
                        calendar.position = calendar_rect.top_left;
                        calendar.size = calendar_rect.size;
                        calendar.draw(display);
                    }
                }else if(wifi_finish){
                    let _ = Text::new("正在同步时间...", Point::new(0, 200), style.clone())
                        .draw(display);
                }
                RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;

            }
        }
    }

    async fn run(&mut self, spawner: Spawner) {
        self.running = true;
        if let None = self.weather_data{
            //self.request().await;
        }
        let mut last_refresh_time = Instant::now();
        self.need_render = true;
        let mut wait_sync_time =true;
        loop {

            if !self.running {
                break;
            }
            
            if sync_time_success() {
                
                if Instant::now().duration_since(last_refresh_time).as_secs() > 60 || wait_sync_time {
                    wait_sync_time = false;
                    self.need_render = true;
                    last_refresh_time = Instant::now();
                } 
            }else{
                if Instant::now().duration_since(last_refresh_time).as_secs() > 5 {
                    self.need_render = true;
                    last_refresh_time = Instant::now();
                }
            }
            self.render().await;

            Timer::after(Duration::from_millis(50)).await;
        }
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |ptr| {
            return Box::pin(async move {
                if let Some(weather) = get_weather() {
                    weather.request().await;
                }
            });
        }).await;
        event::on_target(EventType::KeyShort(3),Self::mut_to_ptr(self),  move |info|  {
            println!("current_page:" );
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.running = false;
            });
        }).await;
    }
}

