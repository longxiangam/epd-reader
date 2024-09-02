use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::Drawable;
use embedded_graphics::pixelcolor::PixelColor;
use embedded_graphics::prelude::{Point, Primitive, Size};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use crate::widgets::scroll_bar::ScrollBarDirection::Horizontal;

#[derive(Eq, PartialEq)]
pub enum  ScrollBarDirection{
    Horizontal,
    Vertical
}
pub struct ScrollBar<C>{
    p_wrap_len:u32,
    p_content_len:u32,
    p_offset:i32,
    out_width:u32,
    inner_bar_width:u32,
    position:Point,
    size:Size,
    front_color:C,
    back_color:C,
    direction:ScrollBarDirection
}


impl <C> ScrollBar<C>{
    ///bar_len 滚动条整个的长度
    ///out_width 滚动条整个的宽度
    ///bar_width 内部条的宽度
    /// p_wrap_len 父级的可视区域长度，p_content_len 父级的内容全长，p_offset 父级内容相对可视区域向上的偏移
   pub fn new(position: Point,out_bar_len:u32,out_width:u32,inner_bar_width:u32,p_wrap_len:u32,p_content_len:u32,p_offset:i32,direction:ScrollBarDirection,front_color:C,back_color:C)->Self{

       let mut size:Option<Size> = None;
       if direction == Horizontal {
           size = Some( Size::new(out_bar_len,out_width));
       } else {
           size = Some(Size::new(out_width,out_bar_len));
       }
       Self{
           position,
           size:size.unwrap(),
           out_width,
           inner_bar_width,
           p_wrap_len,
           p_content_len,
           p_offset,
           direction,
           front_color,
           back_color,
       }
   }

    fn bar_rectangle(&self)->Rectangle{

        if self.direction == Horizontal {
            let ratio = self.size.height as f32 / self.p_content_len as f32;
            let inner_bar_len = (self.p_wrap_len as f32 * ratio) as u32 -1;
            let offset =  ((0-self.p_offset) as f32 *ratio) as u32 +1;
            let left_top = Point::new(offset as i32, self.position.y + (self.out_width as i32 - self.inner_bar_width as i32) / 2 );
            return Rectangle::new(left_top,Size::new(inner_bar_len,self.inner_bar_width));
        }else{
            let ratio =  self.size.height as f32 / self.p_content_len as f32 ;
            let inner_bar_len = (self.p_wrap_len as f32 * ratio) as u32 -1;
            let offset =  ( (0 - self.p_offset) as f32 *ratio) as u32 +1;
            let left_top = Point::new(self.position.x + (self.out_width as i32 - self.inner_bar_width as i32) / 2 ,offset as i32);
            return Rectangle::new(left_top,Size::new(self.inner_bar_width,inner_bar_len));
        }
    }
}


impl <C> Drawable for ScrollBar<C> where C:PixelColor{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, target: &mut D) -> Result<Self::Output, D::Error> where D: DrawTarget<Color=Self::Color> {
        let line_style = PrimitiveStyleBuilder::new()
            .stroke_color(self.front_color)
            .stroke_alignment(StrokeAlignment::Inside)
            .stroke_width(1).build();
        let fill_style = PrimitiveStyleBuilder::new()
            .fill_color(self.front_color)
            .build();
        let _rectangle = Rectangle::new(self.position,self.size)
            .into_styled(line_style)
            .draw(target);

        let rectangle = self.bar_rectangle();
        let _rectangle_inner = rectangle
            .into_styled(fill_style)
            .draw(target);

        Ok(())
    }
}




