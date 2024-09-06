use embassy_executor::Spawner;
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{Dimensions, Point, Size};
use epd_waveshare::color::{Black, Color,White};
use epd_waveshare::graphics::Display;
use esp_println::println;
use futures::FutureExt;
use heapless::{String, Vec};
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::pages::{ Page};
use crate::sd_mount::{SD_MOUNT, SdMount};
use crate::widgets::list_widget::ListWidget;

struct FileItem{
   file_name:String<50>,
}

struct ReadPage{
    running:bool,
    need_render:bool,
    choose_index:u32,
    open_file_name:String<50>,
    menus:Option<Vec<FileItem,20>>,
}

impl Page for ReadPage{
    fn new() -> Self {
        let mut menus = Vec::new();
        Self{
            running: false,
            need_render: false,
            choose_index: 0,
            open_file_name: Default::default(),
            menus: Some(menus),
        }
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(Color::White);

                let menus:Vec<&str,20> = self.menus.as_ref().unwrap().iter().map(|v|{ v.file_name.as_str() }).collect();


                let mut list_widget = ListWidget::new(Point::new(0, 0)
                                                      , Black
                                                      , White
                                                      , Size::new(display.bounding_box().size.height,display.bounding_box().size.width)
                                                      , menus
                );
                list_widget.choose(self.choose_index as usize);
                let _ = list_widget.draw(display);


            }

            RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;
        }
    }

    async fn run(&mut self, spawner: Spawner) {
        //读sd卡目录

        if let Some(ref mut v) =  *SD_MOUNT.lock().await{
            //v.open_root().await.unwrap();

            {

                //let b = v.get_open_root();

            }

        }

/*        let take = SD_MOUNT.lock().await.take();
        {
            let mut tamp = take.unwrap();
            {
                tamp.get_open_root();
            }
        }*/

    }

    async fn bind_event(&mut self) {
        todo!()
        //选择对应文件并打开
    }
}