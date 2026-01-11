use alloc::boxed::Box;
use crate::storage::NvsStorage;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::draw_target::DrawTargetExt;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Dimensions, Point, Size};
use embedded_graphics::primitives::Rectangle;
use embedded_text::TextBox;
use epd_waveshare::color::Black;
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use heapless::String;
use crate::pages::Page;
use crate::storage::{ ErrorLogStorage};
use esp_println::println;
use esp_storage::FlashStorageError;
use u8g2_fonts::{fonts, U8g2TextStyle};
use crate::display::{display_mut, RenderInfo, RENDER_CHANNEL};
use crate::event;
use crate::event::EventType;

pub struct DebugPage {
    running: bool,
    need_render:bool,
    error_log: Option<ErrorLogStorage>,

    
}

impl Page for DebugPage {
    fn new() -> Self {
        Self {
            running: false,
            need_render:false,
            error_log: None,
        }
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(White);
                // 获取错误日志
                self.error_log = ErrorLogStorage::read().ok();
                let clipping_area = Rectangle::new(Point::new(0, 20)
                                                   , Size::new(display.bounding_box().size.width,display.bounding_box().size.height - 20));
                let mut clipped_display = display.clipped(&clipping_area);

                let style =
                    U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312, Black);

                let mut has_error = false;
                if let Some(ref error_log) = self.error_log{
                    println!("=== 错误日志 ===");
                    println!("错误计数: {}", error_log.error_count);
                    println!("最后错误: {}", error_log.last_error);
                    println!("================");
                    if error_log.error_count > 0 {
                        let _ = TextBox::new(
                            error_log.last_error.as_str(),
                            clipping_area,
                            style.clone(),
                        )
                            .draw(&mut clipped_display);
                        has_error = true;
                    }
                } 
                

                if !has_error {
                    println!("没有错误日志");
                    let _ = TextBox::new(
                        "没有错误日志",
                        clipping_area,
                        style.clone(),
                    )
                        .draw(&mut clipped_display);
                }
                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
            }
        }
    }

    async fn run(&mut self, _spawner: Spawner) {
        self.running = true;
        self.need_render = true;
        loop {
            if !self.running {
                break;
            }
            self.render().await;
            Timer::after(Duration::from_secs(1)).await;
        }
    }

    async fn bind_event(&mut self) {
        event::on_target(EventType::KeyShort(3),Self::mut_to_ptr(self),  move |info|  {
            println!("current_page:" );
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.running = false;
            });
        }).await;
        event::on_target(EventType::KeyLongEnd(1),Self::mut_to_ptr(self),  move |info|  {
            return Box::pin(async move {
                let mut_ref:&mut Self =  Self::mut_by_ptr(info.ptr.clone()).unwrap();
                let mut error_log = ErrorLogStorage::read();
                match error_log {
                    Ok( mut v) => {
                        v.error_count = 0;
                        v.last_error.clear();
                        v.write();
                    }
                    _ => {}
                }
                mut_ref.need_render = true;

            });
        }).await;
    }
}
