use alloc::vec;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::prelude::{ PixelColor, Primitive };

use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
use heapless::String;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};
use time::Date;

#[derive(Eq, PartialEq)]
pub struct QrcodeWidget<'a,C>{
    pub str:&'a str,
    position:Point,
    size:Size,
    front_color:C,
    back_color:C,
}

impl <'a,C> QrcodeWidget<'a,C>{
    pub fn new(str:&'a str,position:Point,size: Size,front_color:C,back_color:C)->Self{
        Self{
            str,
            position,
            size,
            front_color,
            back_color,
        }
    }
}

impl <C> Drawable for QrcodeWidget<'_,C> where C:PixelColor{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, display: &mut D) -> Result<Self::Output, D::Error> where D: DrawTarget<Color=Self::Color> {

        let mut outbuffer = vec![0u8; Version::MAX.buffer_len()];
        let mut tempbuffer = vec![0u8; Version::MAX.buffer_len()];
        let qr = QrCode::encode_text(self.str,
                                     &mut tempbuffer, &mut outbuffer, QrCodeEcc::Medium,
                                     Version::MIN, Version::MAX, None, true).unwrap();


        let point_size =  self.size.height as i32  / qr.size() ;


        let mut rectangle = Rectangle::new(Point::new(0,0),Size::new(point_size as u32,point_size as u32));
        let black_style = PrimitiveStyleBuilder::new()
            .fill_color(self.front_color)
            .build();
        let white_style = PrimitiveStyleBuilder::new()
            .fill_color(self.back_color)
            .build();


        for row in 0 .. qr.size() {
            for col in  0 .. qr.size() {

                rectangle.top_left = self.position + Point::new( row * point_size,col * point_size);
                if qr.get_module(row,col) {
                    rectangle.into_styled(black_style).draw(display);
                } else {
                    rectangle.into_styled(white_style).draw(display);
                }
            }
        }

        Ok(())
    }
}