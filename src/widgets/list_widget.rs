
use heapless::{String};
use heapless::Vec;
use core::marker::PhantomData;
use core::str::FromStr;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::Drawable;
use embedded_graphics::prelude::{PixelColor, Point, Primitive, Size};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use u8g2_fonts::{FontRenderer};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::widgets::scroll_bar::{ScrollBar, ScrollBarDirection};

const ITEM_HEIGHT:u32 = 20;
const SCROLL_WIDTH:u32 = 10;
//每个widget 包含状态与绘制，widget 没有业务逻辑只有通过对应事件回调触发
pub struct ListWidget<C>{
    position:Point,
    offset_position:Point,
    size:Size,
    front_color:C,
    back_color:C,
    choose_index:usize,
    items:Vec<ListItemWidget<C>,20>,
}


impl <C: Clone> ListWidget<C>{
    pub fn new(position: Point,front_color:C,back_color:C,size: Size,items:Vec<&str,20>) ->Self{
        let mut list_items = Vec::new();
        let item_size = Size::new(size.width - SCROLL_WIDTH,ITEM_HEIGHT);
        for (index,item) in items.iter().enumerate() {
            let item_position =  Point::new(position.x,position.y + (index) as i32 * ITEM_HEIGHT as i32 );
            let list_item = ListItemWidget::new(item_position,front_color.clone(),back_color.clone(),item_size,String::from_str(item).unwrap());
            let _ = list_items.push(list_item);
        }
        Self{
            position ,
            offset_position:position ,
            size,
            front_color,
            back_color,
            choose_index: 0,
            items: list_items,
        }
    }

    pub fn choose(&mut self,index:usize){
        if index > self.items.len() {
            return;
        }

        for (i, item) in self.items.iter_mut().enumerate() {
            if i == index {
                item.is_choose = true;
            } else {
                item.is_choose = false;
            }
        }

        self.choose_index = index;
        let offset_position = self.offset_by_choose(self.choose_index);
        self.offset_position = offset_position;

        let positions: Vec<Point,20> = (0..self.items.len())
            .map(|index| self.item_position(index))
            .collect();
        for (index, item) in self.items.iter_mut().enumerate() {
            item.set_position(positions[index]);
        }

    }


    pub fn content_height(&self)->u32{
        return self.items.len() as u32 * ITEM_HEIGHT;
    }

    pub fn offset_by_choose(&self,index:usize)->Point{
        if self.content_height() < self.size.height {
            return self.position;
        }

        //最后一个项显示需要偏移的高度
        let last_item_can_show_y =   self.content_height() - self.size.height   ;


       let item_y =  index as u32 * ITEM_HEIGHT;
        //选择的item 的y 坐标大于 中间坐标时 向上移动 这个item - 半屏的高度
        if item_y > self.size.height / 2 {

            //偏移大于最后一个项能显示的高度时，直接用最后一项能显示的偏移
            if item_y > last_item_can_show_y + self.size.height /2 {
                return self.position - Point::new(0, last_item_can_show_y as i32);
            }


           return self.position - Point::new(0, (item_y - self.size.height /2 ) as i32);
        }
        self.position
    }

    pub fn item_position(&self,index:usize)->Point{
        let offset_position = self.offset_by_choose(self.choose_index);
        let item_position = Point::new(offset_position.x,offset_position.y + (index) as i32 * ITEM_HEIGHT as i32 );
        return item_position;
    }


    pub fn item_len(&self)->usize{
        return self.items.len();
    }

}

impl <C> Drawable for  ListWidget<C> where C:PixelColor{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, target: &mut D) -> Result<Self::Output, D::Error>
        where D: DrawTarget<Color=Self::Color> {
        let line_style = PrimitiveStyleBuilder::new()
            .stroke_color(self.front_color)
            .stroke_alignment(StrokeAlignment::Inside)
            .stroke_width(1).build();
        let _rectangle = Rectangle::new(self.position,self.size)
            .into_styled(line_style)
            .draw(target);

        for item in self.items.iter() {
            let _ = item.draw(target);
        }

        let position = Point::new( self.position.x + (self.size.width - SCROLL_WIDTH ) as i32,self.position.y);
        let scroll_bar = ScrollBar::new(position, self.size.height
                                        , SCROLL_WIDTH, 6, self.size.height
                                        , self.content_height()
                                        , self.offset_position.y
                                        , ScrollBarDirection::Vertical
                ,self.front_color,self.back_color
        );
        let _ = scroll_bar.draw(target);

        Ok(())
    }
}


pub struct ListItemWidget<C>{
    label:String<20>,
    position:Point,
    size:Size,
    front_color:C,
    back_color:C,
    is_choose:bool,
    _marker:PhantomData<C>,
}

impl <C: Clone>ListItemWidget<C>{
    fn new(position: Point,front_color:C,back_color:C,size: Size,label:String<20>) ->Self{

        Self{
            label,
            position ,
            size,
            front_color,
            back_color,
            is_choose: false,
            _marker: Default::default(),
        }

    }
}

impl <C> ListItemWidget<C>{
    pub fn set_position(&mut self,position:Point){
        self.position = position;
    }
}

impl <C> Drawable for  ListItemWidget<C>  where C:PixelColor{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, target: &mut D) -> Result<Self::Output, D::Error>
        where D: DrawTarget<Color=Self::Color> {

        if self.is_choose {
            let line_style = PrimitiveStyleBuilder::new()
                .stroke_color(self.front_color)
                .stroke_alignment(StrokeAlignment::Inside)
                .fill_color(self.front_color)
                .stroke_width(1).build();
            let _rectangle = Rectangle::new(self.position,self.size)
                .into_styled(line_style)
                .draw(target);

        }

       /* let mut style =
            U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, self.front_color);
        Text::new(self.label, self.position, style )
            .draw(target);*/

        let mut tag = "-";
        if self.is_choose {
            tag = "*";
        }


        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();
        let _ = font.render_aligned(
            format_args!("{} {}",tag, self.label),
            self.position+Point::new(10,5),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(if self.is_choose { self.back_color}else {self.front_color}),
            target,
        );
        //style.set_background_color(Some(self.back_color));


        //一个中间有方案的矩形
        Ok(())
    }
}




pub trait Widget:Drawable{

}