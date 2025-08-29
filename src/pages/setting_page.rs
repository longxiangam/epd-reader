use alloc::{format, vec};
use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Dimensions, OriginDimensions, Point, Size};
use embedded_graphics::prelude::{DrawTarget, DrawTargetExt, Primitive};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use embedded_text::TextBox;
use esp_hal::reset::software_reset;
use heapless::String;
use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use epd_waveshare::color::{Black, Color};
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::pages::Page;
use crate::storage::{init_storage_area, NvsStorage, WIFI_INFO};
use crate::widgets::qrcode_widget::QrcodeWidget;
use crate::wifi::{finish_wifi, IP_ADDRESS,  use_wifi,  WIFI_MODEL, WifiNetError};
use crate::web_service::{web_service,STOP_WEB_SERVICE};

pub struct SettingPage {
    need_render:bool,
    running:bool,
    long_start_time:u64,
    ip:String<20>
}

impl SettingPage {


}

impl Page for SettingPage {
    fn new() -> Self {
        Self{
            need_render: false,
            running: false,
            long_start_time: 0,
            ip: Default::default(),
        }
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(White);
                let ip = unsafe { &IP_ADDRESS };
                let mut url:String<50> = String::new();
                url.push_str("http://");
                url.push_str(ip);
                url.push_str(":80/config");

                let qrcode_widget = QrcodeWidget::new(&url,Point::new(0,0)
                                                      , Size::new(display.bounding_box().size.height,display.bounding_box().size.height)
                                                      , Black, epd_waveshare::color::White);
                qrcode_widget.draw(display);

                let style =
                    U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, Black);
                let _ = Text::new("长按旋钮5秒重置设备", Point::new(display.bounding_box().size.height as i32, 20), style.clone())
                    .draw(display);

                let clipping_area = Rectangle::new(Point::new(display.bounding_box().size.height as i32, 40)
                                                   , Size::new(display.bounding_box().size.width - display.bounding_box().size.height,60));
                let mut clipped_display = display.clipped(&clipping_area);

                TextBox::new(
                    format!("地址：{}",url).as_str(),
                    clipping_area,
                    style.clone(),
                )
                    .draw(&mut clipped_display);


                if self.long_start_time > 0 {
                    let secs =Instant::now().as_secs() - self.long_start_time;
                    let _ = Text::new( format!("已长按：{} 秒",secs).as_str(), Point::new(display.bounding_box().size.height as i32, 80), style.clone())
                        .draw(display);
                }


                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
            }
        }
    }

    async fn run(&mut self, spawner: Spawner) {
        spawner.spawn(web_service()).ok();
        self.running = true;
        let mut last_time = 0 ;
        self.need_render = true;
        loop {
            if !self.running {
                break;
            }
            crate::wifi::refresh_last_time().await;
         
            self.render().await;
            Timer::after(Duration::from_millis(1000)).await;
        }

        STOP_WEB_SERVICE.signal(());

    }

    async fn bind_event(&mut self) {
        event::clear().await;

        event::on_target(EventType::KeyShort(3),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.running = false;
            });
        }).await;


        event::on_target(EventType::KeyLongStart(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr.clone()).unwrap();
                mut_ref.long_start_time = Instant::now().as_secs();
            });
        }).await;

        event::on_target(EventType::KeyLongIng(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr.clone()).unwrap();
                if(mut_ref.long_start_time == 0){
                    mut_ref.long_start_time = Instant::now().as_secs();
                }
                if(Instant::now().as_secs() - mut_ref.long_start_time  > 5){
                    init_storage_area();
                    software_reset();
                }
            });
        }).await;
        event::on_target(EventType::KeyLongEnd(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr.clone()).unwrap();
                mut_ref.long_start_time = 0;
            });
        }).await;


    }
}

