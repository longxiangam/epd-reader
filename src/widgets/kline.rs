use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{DrawTarget, Primitive};
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::Point;
use epd_waveshare::color::Black;

use crate::model::stock::KLine;

/// 价格 -> 屏幕 Y。屏幕坐标向下递增，故高价在上。
fn map_y(price: f32, lo: f32, hi: f32, top: i32, h: i32) -> i32 {
    let range = hi - lo;
    let r = if range > 0.0 { range } else { 1.0 };
    let frac = (hi - price) / r;
    top + (frac * (h as f32 - 1.0)) as i32
}

/// 计算价格区间并预留 5% 边距，返回 (lo, hi)
fn padded_range(lo: f32, hi: f32) -> (f32, f32) {
    let hi = if hi > lo { hi } else { lo + 1.0 };
    let pad = (hi - lo) * 0.05;
    (lo - pad, hi + pad)
}

/// 蜡烛图。二值屏：涨(收>=开)=空心，跌=实心，影线 1px。
pub fn draw_candles<D>(display: &mut D, area: Rectangle, klines: &[KLine]) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    if klines.is_empty() {
        return Ok(());
    }
    let left = area.top_left.x;
    let top = area.top_left.y;
    let w = area.size.width as i32;
    let h = area.size.height as i32;
    if w < 4 || h < 4 {
        return Ok(());
    }

    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for k in klines {
        if k.low < lo { lo = k.low; }
        if k.high > hi { hi = k.high; }
    }
    let (lo, hi) = padded_range(lo, hi);

    let n = klines.len() as i32;
    let slot = w as f32 / n as f32;
    let body_w = ((slot * 0.7) as i32).max(2);

    let wick = PrimitiveStyleBuilder::new().stroke_color(Black).stroke_width(1).build();
    let hollow = PrimitiveStyleBuilder::new()
        .stroke_color(Black)
        .stroke_width(1)
        .stroke_alignment(StrokeAlignment::Inside)
        .build();
    let filled = PrimitiveStyleBuilder::new()
        .stroke_color(Black)
        .fill_color(Black)
        .stroke_width(1)
        .stroke_alignment(StrokeAlignment::Inside)
        .build();

    for (i, k) in klines.iter().enumerate() {
        let cx = left + (slot * (i as f32 + 0.5)) as i32;
        let y_high = map_y(k.high, lo, hi, top, h);
        let y_low = map_y(k.low, lo, hi, top, h);
        let y_open = map_y(k.open, lo, hi, top, h);
        let y_close = map_y(k.close, lo, hi, top, h);

        // 影线
        Line::new(Point::new(cx, y_high), Point::new(cx, y_low))
            .into_styled(wick)
            .draw(display)?;

        // 实体（至少 1px，处理开≈收的一字线）
        let top_body = y_open.min(y_close);
        let bot_body = y_open.max(y_close);
        let bh = (bot_body - top_body).max(1);
        let bx = cx - body_w / 2;
        let body = Rectangle::with_corners(
            Point::new(bx, top_body),
            Point::new(bx + body_w - 1, top_body + bh - 1),
        );
        if k.close >= k.open {
            body.into_styled(hollow).draw(display)?;
        } else {
            body.into_styled(filled).draw(display)?;
        }
    }
    Ok(())
}

/// 折线图（分时 / 折线模式），逐段 Line 连接。
pub fn draw_line<D>(display: &mut D, area: Rectangle, prices: &[f32]) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    if prices.len() < 2 {
        return Ok(());
    }
    let left = area.top_left.x;
    let top = area.top_left.y;
    let w = area.size.width as i32;
    let h = area.size.height as i32;
    if w < 4 || h < 4 {
        return Ok(());
    }

    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for &p in prices {
        if p < lo { lo = p; }
        if p > hi { hi = p; }
    }
    let (lo, hi) = padded_range(lo, hi);

    let n = prices.len() as i32;
    let style = PrimitiveStyleBuilder::new().stroke_color(Black).stroke_width(1).build();

    let mut prev: Option<Point> = None;
    for (i, &p) in prices.iter().enumerate() {
        let x = left + (w as f32 * i as f32 / (n as f32 - 1.0)) as i32;
        let y = map_y(p, lo, hi, top, h);
        if let Some(p0) = prev {
            Line::new(p0, Point::new(x, y)).into_styled(style).draw(display)?;
        }
        prev = Some(Point::new(x, y));
    }
    Ok(())
}
