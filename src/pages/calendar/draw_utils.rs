use embedded_graphics::Drawable;
use embedded_graphics::prelude::{DrawTarget, Point, Primitive};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyleBuilder};

pub fn draw_wifi_status<D>(position: Point, target: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(BinaryColor::On)
        .stroke_width(1)
        .build();
    let dot = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::On)
        .build();

    let x = position.x;
    let y = position.y;

    let state = embassy_futures::block_on(crate::wifi::WIFI_STATE.lock());
    let connected = matches!(state.as_ref(), Some(crate::wifi::WifiNetState::WifiConnected));

    Line::new(Point::new(x - 3, y), Point::new(x + 3, y))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 6, y - 3), Point::new(x + 6, y - 3))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 9, y - 6), Point::new(x + 9, y - 6))
        .into_styled(stroke.clone()).draw(target);
    Circle::new(Point::new(x - 1, y + 2), 3)
        .into_styled(dot.clone()).draw(target);

    if !connected {
        Line::new(Point::new(x - 7, y - 7), Point::new(x + 7, y + 5))
            .into_styled(stroke).draw(target);
    }
}

pub fn draw_loading_icon<D>(position: Point, target: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(BinaryColor::On)
        .stroke_width(1)
        .build();

    let x = position.x;
    let y = position.y;

    Line::new(Point::new(x, y - 7), Point::new(x, y - 1))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 3, y - 4), Point::new(x, y - 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x + 3, y - 4), Point::new(x, y - 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x, y + 1), Point::new(x, y + 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 3, y + 4), Point::new(x, y + 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x + 3, y + 4), Point::new(x, y + 7))
        .into_styled(stroke).draw(target);
}

pub fn draw_moon_icon<D>(position: Point, target: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::On)
        .build();
    let erase = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::Off)
        .build();

    let r: u32 = 7;
    let cx = position.x + r as i32;
    let cy = position.y + r as i32;

    let _ = Circle::new(Point::new(cx - r as i32, cy - r as i32), r * 2)
        .into_styled(fill)
        .draw(target);
    let _ = Circle::new(Point::new(cx - r as i32 + 5, cy - r as i32 - 2), r * 2)
        .into_styled(erase)
        .draw(target);
}
