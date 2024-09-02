use alloc::format;
use alloc::string::ToString;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::Point;
use embedded_graphics::prelude::{DrawTargetExt, PixelColor, Primitive, Size};
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use embedded_graphics::text::renderer::CharacterStyle;
use embedded_layout::View;
use time::{Date, Month};
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::fonts;

#[derive(Eq, PartialEq)]
pub struct Calendar<C> {
    pub position: Point,
    pub size: Size,
    month_first_day: Date,
    month_last_day: Date,
    today: Date,
    front_color: C,
    back_color: C,
}

impl<C> Calendar<C> {
    pub fn new(
        position: Point,
        size: Size,
        year: i32,
        month: Month,
        today: Date,
        front_color: C,
        back_color: C,
    ) -> Self {
        let first_day = Date::from_calendar_date(year, month, 1).unwrap();
        let last_day = (first_day + time::Duration::days(31))
            .replace_day(1)
            .unwrap()
            .previous_day()
            .unwrap();

        Self {
            position,
            size,
            month_first_day: first_day,
            month_last_day: last_day,
            today,
            front_color,
            back_color,
        }
    }

    pub fn set_date_of_month(&mut self, year: i32, month: Month) {
        let first_day = Date::from_calendar_date(year, month, 1).unwrap();
        let last_day = (first_day + time::Duration::days(31))
            .replace_day(1)
            .unwrap()
            .previous_day()
            .unwrap();
        self.month_first_day = first_day;
        self.month_last_day = last_day;
    }
}

impl<C> Drawable for Calendar<C>
    where
        C: PixelColor,
{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, display: &mut D) -> Result<Self::Output, D::Error>
        where
            D: DrawTarget<Color = Self::Color>,
    {
        // 裁剪区域
        let clipping_area = Rectangle::new(self.position, self.size);
        // 使用 display.clipped 包裹裁剪区域
        let mut clipped_display = display.clipped(&clipping_area);

        let style = U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, self.front_color);
        let text_style = TextStyleBuilder::new().baseline(Baseline::Middle)
            .alignment(Alignment::Center).build();

        let line_style = PrimitiveStyleBuilder::new()
            .stroke_color(self.front_color)
            .stroke_alignment(StrokeAlignment::Inside)
            .stroke_width(1)
            .build();

        let year = self.month_first_day.year();
        let month = self.month_first_day.month();
        let title_height = 12;

        let title_rect = Rectangle::new(self.position,Size::new(self.size.width,title_height));
        // 绘制月份和年份
        let month_year = format!("{}-{}", year, month as u8);
        Text::with_text_style(&month_year, title_rect.center(), style.clone(), text_style)
            .draw(&mut clipped_display)?;

        //计算小格大小与位置
        let grid_width = self.size.width / 7; //7列
        let grid_height = ( self.size.height - title_height ) / 7;//加表头7行
        let mut rect = Rectangle::new(Point::zero(), Size::new(grid_width, grid_height));



        // 绘制星期标题
        let days = ["日", "一", "二", "三", "四", "五", "六"];
        let top = grid_height ;

        for (i, &day) in days.iter().enumerate() {
            rect.top_left = self.position + Point::new(i as i32 * grid_width as i32, top as i32);
            Text::with_text_style(day,rect.center(), style.clone(), text_style)
                .draw(&mut clipped_display)?;
        }


        // 获取当月的第一天和最后一天
        let first_day = Date::from_calendar_date(year, month, 1).unwrap();
        let last_day = (first_day + time::Duration::days(31))
            .replace_day(1)
            .unwrap()
            .previous_day()
            .unwrap();
        let mut same_month = false;
        let today_day = self.today.day();
        if first_day.year() == self.today.year() && first_day.month() == self.today.month() {
            same_month = true;
        }

        // 绘制日期
        let mut x = first_day.weekday().number_days_from_sunday() as i32 * grid_width as i32;
        let mut y =  top as i32 + grid_height as i32;


        for day in 1..=last_day.day() {

            rect.top_left = self.position + Point::new(x, y);
            let mut grid = rect.clone();
            grid.top_left = rect.top_left - Point::new(0,2);
            grid.size = rect.size + Size::new(1,1);
            let _rectangle =  grid
                .into_styled(line_style)
                .draw(&mut clipped_display);

            //当天反显
            if same_month && day == today_day {
                let line_style = PrimitiveStyleBuilder::new()
                    .stroke_color(self.front_color)
                    .stroke_alignment(StrokeAlignment::Inside)
                    .fill_color(self.front_color)
                    .stroke_width(1).build();
                grid .into_styled(line_style)
                    .draw(&mut clipped_display);
                let mut temp_style = style.clone();
                temp_style.set_text_color(Some(self.back_color));
                Text::with_text_style(&day.to_string(), rect.top_left + Point::new( 8 , (grid_height / 2) as i32), temp_style, text_style)
                    .draw(&mut clipped_display)?;

            }else{
                Text::with_text_style(&day.to_string(), rect.top_left + Point::new( 8 , (grid_height / 2) as i32), style.clone(), text_style)
                    .draw(&mut clipped_display)?;
            }

            x += grid_width as i32;
            if x >= (grid_width * 7) as i32 {
                x = 0;
                y += grid_height as i32;
            }
        }
        Ok(())
    }
}
