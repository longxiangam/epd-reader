use embedded_graphics::Drawable;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::prelude::{PixelColor, Point, Primitive, Size};
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

/// 温度折线图数据点
pub struct TempPoint {
    pub label: &'static str,
    pub high: i32,
    pub low: i32,
}

/// 绘制温度折线图
/// - position: 图表区域左上角
/// - chart_size: 图表绘制区域大小（不含标签）
/// - points: 各天温度数据
/// - color: 前景色
/// - high_label: 高温线标签（如 "高"）
/// - low_label: 低温线标签（如 "低"）
pub fn draw_temp_chart<C, D>(
    position: Point,
    chart_size: Size,
    points: &[TempPoint],
    color: C,
    target: &mut D,
) -> Result<(), D::Error>
where
    C: PixelColor + Clone,
    D: DrawTarget<Color = C>,
{
    if points.is_empty() {
        return Ok(());
    }

    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(color.clone())
        .stroke_width(1)
        .build();

    let thick_stroke = PrimitiveStyleBuilder::new()
        .stroke_color(color.clone())
        .stroke_width(2)
        .build();

    let dot_fill = PrimitiveStyleBuilder::new()
        .fill_color(color.clone())
        .build();

    // 温度范围
    let mut temp_min = i32::MAX;
    let mut temp_max = i32::MIN;
    for p in points {
        if p.high > temp_max { temp_max = p.high; }
        if p.low > temp_max { temp_max = p.low; }
        if p.high < temp_min { temp_min = p.high; }
        if p.low < temp_min { temp_min = p.low; }
    }
    // 上下留一格余量
    temp_min -= 2;
    temp_max += 2;
    let temp_range = (temp_max - temp_min).max(1);

    let chart_x = position.x;
    let chart_y = position.y;
    let chart_w = chart_size.width as i32;
    let chart_h = chart_size.height as i32;

    let count = points.len() as i32;
    let step_x = if count > 1 { chart_w / (count - 1) } else { 0 };

    // Y 坐标映射：temp → pixel
    let temp_to_y = |temp: i32| -> i32 {
        chart_y + chart_h - ((temp - temp_min) * chart_h / temp_range)
    };

    // 水平参考线（每隔 5℃ 画一条淡线）
    {
        let mut t = (temp_min / 5) * 5 + 5;
        while t < temp_max {
            let y = temp_to_y(t);
            Line::new(
                Point::new(chart_x, y),
                Point::new(chart_x + chart_w, y),
            )
            .into_styled(stroke.clone())
            .draw(target)?;
            t += 5;
        }
    }

    // 计算各点坐标
    let mut high_pts: heapless::Vec<Point, 10> = heapless::Vec::new();
    let mut low_pts: heapless::Vec<Point, 10> = heapless::Vec::new();
    for (i, p) in points.iter().enumerate() {
        let x = if count > 1 { chart_x + i as i32 * step_x } else { chart_x + chart_w / 2 };
        let _ = high_pts.push(Point::new(x, temp_to_y(p.high)));
        let _ = low_pts.push(Point::new(x, temp_to_y(p.low)));
    }

    // 绘制高温折线
    draw_polyline(&high_pts, thick_stroke.clone(), dot_fill.clone(), target)?;
    // 绘制低温折线
    draw_polyline(&low_pts, thick_stroke, dot_fill, target)?;

    Ok(())
}

/// 绘制温度标签（在图表上方和下方）
pub fn draw_temp_labels<C, D>(
    position: Point,
    chart_size: Size,
    points: &[TempPoint],
    color: C,
    font: &FontRenderer,
    target: &mut D,
) -> Result<(), D::Error>
where
    C: PixelColor + Clone,
    D: DrawTarget<Color = C>,
{
    if points.is_empty() {
        return Ok(());
    }

    let mut temp_min = i32::MAX;
    let mut temp_max = i32::MIN;
    for p in points {
        if p.high > temp_max { temp_max = p.high; }
        if p.low > temp_max { temp_max = p.low; }
        if p.high < temp_min { temp_min = p.high; }
        if p.low < temp_min { temp_min = p.low; }
    }
    temp_min -= 2;
    temp_max += 2;
    let temp_range = (temp_max - temp_min).max(1);

    let chart_x = position.x;
    let chart_y = position.y;
    let chart_w = chart_size.width as i32;
    let chart_h = chart_size.height as i32;
    let count = points.len() as i32;
    let step_x = if count > 1 { chart_w / (count - 1) } else { 0 };

    let temp_to_y = |temp: i32| -> i32 {
        chart_y + chart_h - ((temp - temp_min) * chart_h / temp_range)
    };

    for (i, p) in points.iter().enumerate() {
        let x = if count > 1 { chart_x + i as i32 * step_x } else { chart_x + chart_w / 2 };

        // 高温标签（点上方）
        let high_y = temp_to_y(p.high);
        let _ = font.render_aligned(
            format_args!("{}℃", p.high),
            Point::new(x, high_y - 12),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(color.clone()),
            target,
        );

        // 低温标签（点下方）
        let low_y = temp_to_y(p.low);
        let _ = font.render_aligned(
            format_args!("{}℃", p.low),
            Point::new(x, low_y + 2),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(color),
            target,
        );
    }

    Ok(())
}

fn draw_polyline<C, D>(
    pts: &[Point],
    line_style: PrimitiveStyle<C>,
    dot_style: PrimitiveStyle<C>,
    target: &mut D,
) -> Result<(), D::Error>
where
    C: PixelColor + Clone,
    D: DrawTarget<Color = C>,
{
    if pts.is_empty() {
        return Ok(());
    }
    for i in 1..pts.len() {
        Line::new(pts[i - 1], pts[i])
            .into_styled(line_style.clone())
            .draw(target)?;
    }
    for &p in pts {
        Circle::new(Point::new(p.x - 2, p.y - 2), 4)
            .into_styled(dot_style.clone())
            .draw(target)?;
    }
    Ok(())
}
