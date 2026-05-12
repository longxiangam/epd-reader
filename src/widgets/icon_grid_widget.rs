
use heapless::String;
use heapless::Vec;
use core::str::FromStr;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::prelude::{PixelColor, Point, Primitive, Size};
use embedded_graphics::primitives::{Circle, CornerRadii, Line, PrimitiveStyleBuilder, Rectangle, RoundedRectangle, StrokeAlignment};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::pages::IconType;

const ICON_SIZE: u32 = 28;
const TITLE_HEIGHT: u32 = 16;
const CELL_PADDING: u32 = 4;

pub struct IconGridWidget<C> {
    position: Point,
    size: Size,
    columns: usize,
    front_color: C,
    back_color: C,
    choose_index: usize,
    cells: Vec<GridCell<C>, 20>,
}

struct GridCell<C> {
    icon_type: IconType,
    label: String<20>,
    position: Point,
    cell_size: Size,
    front_color: C,
    back_color: C,
    is_choose: bool,
}

impl<C: Clone> GridCell<C> {
    fn new(
        icon_type: IconType,
        label: String<20>,
        position: Point,
        cell_size: Size,
        front_color: C,
        back_color: C,
    ) -> Self {
        Self {
            icon_type,
            label,
            position,
            cell_size,
            front_color,
            back_color,
            is_choose: false,
        }
    }

    fn icon_center(&self) -> Point {
        let icon_area_y = self.position.y + ((self.cell_size.height - TITLE_HEIGHT - ICON_SIZE) / 2) as i32;
        Point::new(
            self.position.x + self.cell_size.width as i32 / 2,
            icon_area_y + ICON_SIZE as i32 / 2,
        )
    }

    fn title_position(&self) -> Point {
        Point::new(
            self.position.x + self.cell_size.width as i32 / 2,
            self.position.y + self.cell_size.height as i32 - TITLE_HEIGHT as i32 / 2,
        )
    }
}

impl<C: Clone> IconGridWidget<C> {
    pub fn new(
        position: Point,
        front_color: C,
        back_color: C,
        size: Size,
        columns: usize,
        items: Vec<(IconType, &str), 20>,
    ) -> Self {
        let cell_width = size.width / columns as u32;
        let cell_height = size.height / ((items.len() + columns - 1) / columns) as u32;
        let cell_size = Size::new(cell_width, cell_height);

        let mut cells = Vec::new();
        for (index, (icon, label)) in items.iter().enumerate() {
            let col = index % columns;
            let row = index / columns;
            let cell_pos = Point::new(
                position.x + col as i32 * cell_width as i32,
                position.y + row as i32 * cell_height as i32,
            );
            let cell = GridCell::new(
                *icon,
                String::from_str(label).unwrap_or_default(),
                cell_pos,
                cell_size,
                front_color.clone(),
                back_color.clone(),
            );
            let _ = cells.push(cell);
        }

        Self {
            position,
            size,
            columns,
            front_color,
            back_color,
            choose_index: 0,
            cells,
        }
    }

    pub fn choose(&mut self, index: usize) {
        if index >= self.cells.len() {
            return;
        }
        for (i, cell) in self.cells.iter_mut().enumerate() {
            cell.is_choose = i == index;
        }
        self.choose_index = index;
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }
}

impl<C> Drawable for IconGridWidget<C>
where
    C: PixelColor + Clone,
{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, target: &mut D) -> Result<Self::Output, D::Error>
    where
        D: DrawTarget<Color = Self::Color>,
    {
        for cell in self.cells.iter() {
            let _ = cell.draw(target);
        }
        Ok(())
    }
}

impl<C> Drawable for GridCell<C>
where
    C: PixelColor,
{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, target: &mut D) -> Result<Self::Output, D::Error>
    where
        D: DrawTarget<Color = Self::Color>,
    {
        let stroke = PrimitiveStyleBuilder::new()
            .stroke_color(self.front_color)
            .stroke_alignment(StrokeAlignment::Inside)
            .stroke_width(1)
            .build();

        let border_style = PrimitiveStyleBuilder::new()
            .stroke_color(self.front_color)
            .stroke_alignment(StrokeAlignment::Inside)
            .stroke_width(2)
            .build();

        if self.is_choose {
            let _ = RoundedRectangle::new(
                Rectangle::new(self.position, self.cell_size),
                CornerRadii::new(Size::new(6, 6)),
            )
            .into_styled(border_style)
            .draw(target);
        }

        let icon_color = self.front_color.clone();
        let title_color = self.front_color.clone();

        draw_icon(self.icon_type, self.icon_center(), icon_color, target)?;

        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>();
        let _ = font.render_aligned(
            format_args!("{}", self.label.as_str()),
            self.title_position(),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(title_color),
            target,
        );

        Ok(())
    }
}

fn draw_icon<C, D>(icon: IconType, center: Point, color: C, target: &mut D) -> Result<(), D::Error>
where
    C: PixelColor,
    D: DrawTarget<Color = C>,
{
    let style = PrimitiveStyleBuilder::new()
        .stroke_color(color.clone())
        .stroke_width(1)
        .build();

    let fill = PrimitiveStyleBuilder::new()
        .fill_color(color.clone())
        .build();

    let half = (ICON_SIZE / 2) as i32;
    let q = (ICON_SIZE / 4) as i32;

    match icon {
        IconType::Book => {
            // Open book: two angled rectangles
            let spine_top = Point::new(center.x, center.y - half);
            let left_top = Point::new(center.x - half, center.y - q);
            let left_bottom = Point::new(center.x - half, center.y + half);
            let right_top = Point::new(center.x + half, center.y - q);
            let right_bottom = Point::new(center.x + half, center.y + half);
            let spine_bottom = Point::new(center.x, center.y + half);

            Line::new(spine_top, left_top).into_styled(style.clone()).draw(target)?;
            Line::new(left_top, left_bottom).into_styled(style.clone()).draw(target)?;
            Line::new(left_bottom, spine_bottom).into_styled(style.clone()).draw(target)?;
            Line::new(spine_top, right_top).into_styled(style.clone()).draw(target)?;
            Line::new(right_top, right_bottom).into_styled(style.clone()).draw(target)?;
            Line::new(right_bottom, spine_bottom).into_styled(style.clone()).draw(target)?;

            // Horizontal lines for pages
            let ly = center.y;
            Line::new(Point::new(center.x - half + 3, ly), Point::new(center.x - 1, ly))
                .into_styled(style.clone()).draw(target)?;
            Line::new(Point::new(center.x + 1, ly), Point::new(center.x + half - 3, ly))
                .into_styled(style.clone()).draw(target)?;
        }
        IconType::Weather => {
            // Sun: circle with rays
            let sun_r = (ICON_SIZE / 3) as i32;
            Circle::new(Point::new(center.x - sun_r, center.y - sun_r), sun_r as u32 * 2)
                .into_styled(style.clone())
                .draw(target)?;

            let ray_len = 4i32;
            let dirs: [(i32, i32); 8] = [
                (1, 0), (-1, 0), (0, 1), (0, -1),
                (1, 1), (-1, 1), (1, -1), (-1, -1),
            ];
            for (dx, dy) in dirs {
                let start = Point::new(
                    center.x + dx * (sun_r + 2),
                    center.y + dy * (sun_r + 2),
                );
                let end = Point::new(
                    center.x + dx * (sun_r + 2 + ray_len),
                    center.y + dy * (sun_r + 2 + ray_len),
                );
                Line::new(start, end).into_styled(style.clone()).draw(target)?;
            }
        }
        IconType::Calendar => {
            // Calendar: rectangle with header and small "rings" on top
            let left = center.x - half;
            let top = center.y - half + 4;
            let w = ICON_SIZE as i32;
            let h = ICON_SIZE as i32 - 4;
            let header_h = h / 3;

            Rectangle::new(Point::new(left, top), Size::new(w as u32, h as u32))
                .into_styled(style.clone())
                .draw(target)?;

            // Header fill
            Rectangle::new(Point::new(left + 1, top + 1), Size::new((w - 2) as u32, header_h as u32))
                .into_styled(fill)
                .draw(target)?;

            // Rings on top
            for dx in [-q, q] {
                let ring_x = center.x + dx;
                let ring_top = Point::new(ring_x, top - 4);
                let ring_bot = Point::new(ring_x, top + 2);
                Line::new(ring_top, ring_bot).into_styled(style.clone()).draw(target)?;
            }

            // Grid lines in body (2 rows, 3 cols)
            let body_top = top + header_h;
            let row_h = (h - header_h) / 2;
            let col_w = w / 3;
            for row in 1..2 {
                let ly = body_top + row * row_h;
                Line::new(Point::new(left, ly), Point::new(left + w, ly))
                    .into_styled(style.clone()).draw(target)?;
            }
            for col in 1..3 {
                let lx = left + col * col_w;
                Line::new(Point::new(lx, body_top), Point::new(lx, top + h))
                    .into_styled(style.clone()).draw(target)?;
            }
        }
        IconType::Settings => {
            // Gear: outer circle with "teeth", inner circle
            let outer_r = half - 2;
            let inner_r = outer_r / 2;

            Circle::new(
                Point::new(center.x - outer_r, center.y - outer_r),
                outer_r as u32 * 2,
            )
            .into_styled(style.clone())
            .draw(target)?;

            // Teeth as small lines extending from the circle at 6 positions
            let tooth_len = 4i32;
            let positions: [(i32, i32); 6] = [
                (1, 0), (0, 1), (-1, 0), (0, -1), (1, 1), (-1, -1),
            ];
            for (dx, dy) in positions {
                let start = Point::new(
                    center.x + dx * (outer_r - 1),
                    center.y + dy * (outer_r - 1),
                );
                let norm = if dx != 0 && dy != 0 { 1 } else { 1 };
                let end = Point::new(
                    center.x + dx * (outer_r - 1 + tooth_len * norm),
                    center.y + dy * (outer_r - 1 + tooth_len * norm),
                );
                Line::new(start, end)
                    .into_styled(PrimitiveStyleBuilder::new().stroke_color(color.clone()).stroke_width(3).build())
                    .draw(target)?;
            }

            // Inner circle (hole)
            Circle::new(
                Point::new(center.x - inner_r, center.y - inner_r),
                inner_r as u32 * 2,
            )
            .into_styled(fill)
            .draw(target)?;
        }
        IconType::Image => {
            // Photo/picture frame: rectangle with mountain and sun inside
            let left = center.x - half;
            let top = center.y - half;
            let w = ICON_SIZE as i32;
            let h = ICON_SIZE as i32;

            // Frame
            Rectangle::new(Point::new(left, top), Size::new(w as u32, h as u32))
                .into_styled(style.clone())
                .draw(target)?;

            // Sun (small circle, top-right inside)
            let sun_r = 3i32;
            let sun_cx = left + w - 7;
            let sun_cy = top + 7;
            Circle::new(Point::new(sun_cx - sun_r, sun_cy - sun_r), sun_r as u32 * 2)
                .into_styled(style.clone())
                .draw(target)?;

            // Mountain: triangle at bottom
            let peak = Point::new(left + w / 2 - 2, top + h / 3);
            let bl = Point::new(left + 3, top + h - 3);
            let br = Point::new(left + w - 3, top + h - 3);
            Line::new(peak, bl).into_styled(style.clone()).draw(target)?;
            Line::new(peak, br).into_styled(style.clone()).draw(target)?;

            // Second smaller mountain
            let peak2 = Point::new(left + w * 3 / 4 + 2, top + h / 2 + 2);
            let br2 = Point::new(left + w - 3, top + h - 3);
            Line::new(peak2, br2).into_styled(style.clone()).draw(target)?;
        }
        IconType::Debug => {
            // Terminal/console icon: rectangle with ">_" text
            let left = center.x - half;
            let top = center.y - half;
            let w = ICON_SIZE as i32;
            let h = ICON_SIZE as i32;

            Rectangle::new(Point::new(left, top), Size::new(w as u32, h as u32))
                .into_styled(style.clone())
                .draw(target)?;

            // ">_" prompt lines
            let prompt_y = center.y + 2;
            Line::new(Point::new(left + 5, prompt_y - 4), Point::new(left + 10, prompt_y))
                .into_styled(style.clone()).draw(target)?;
            Line::new(Point::new(left + 5, prompt_y + 4), Point::new(left + 10, prompt_y))
                .into_styled(style.clone()).draw(target)?;
            Line::new(Point::new(left + 14, prompt_y), Point::new(left + 18, prompt_y))
                .into_styled(style.clone()).draw(target)?;
        }
    }
    Ok(())
}
