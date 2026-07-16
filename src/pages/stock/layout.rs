use core::fmt::Write;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Point, Primitive, Size};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::Drawable;
use epd_waveshare::color::Black;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::render_data::StockRenderData;
use crate::display::EpdDisplay;
use crate::model::stock::{fmt_price, fmt_signed, ChartMode, KLINE_CAP, StockData};
use crate::widgets::kline::{draw_candles, draw_line};

pub fn draw<D>(display: &mut D, data: &StockRenderData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let w = data.w;
    let h = data.h;

    let font_mid: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();
    let font_small: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>();

    match data.data {
        None => {
            let msg = if data.loading {
                "加载中..."
            } else {
                data.err_msg.unwrap_or("无数据 按1刷新")
            };
            let _ = font_mid.render_aligned(
                format_args!("{}", msg),
                Point::new(w / 2, h / 2),
                VerticalPosition::Top,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
            return Ok(());
        }
        Some(sd) => draw_content(display, data, sd, w, h, &font_mid, &font_small),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_content<D>(
    display: &mut D,
    data: &StockRenderData,
    sd: &StockData,
    w: i32,
    h: i32,
    font_mid: &FontRenderer,
    font_small: &FontRenderer,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let header_h: i32 = 30;
    let bottom_h: i32 = 16;
    let left_pad: i32 = 34; // 左侧留白画价位刻度

    // ---- 顶部信息：代码 | 现价 | 涨跌额 涨跌幅 | 模式 ----
    let _ = font_mid.render_aligned(
        format_args!("{}", sd.code.as_str()),
        Point::new(2, 4),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );

    let price = fmt_price(sd.last_price);
    let _ = font_mid.render_aligned(
        format_args!("{}", price.as_str()),
        Point::new(64, 4),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );

    // 涨跌额 + 涨跌幅%
    let mut chg_line: heapless::String<28> = heapless::String::new();
    let _ = chg_line.push_str(fmt_signed(sd.change).as_str());
    let _ = chg_line.push_str("  ");
    let _ = chg_line.push_str(fmt_signed(sd.change_pct).as_str());
    let _ = chg_line.push_str("%");
    let _ = font_small.render_aligned(
        format_args!("{}", chg_line.as_str()),
        Point::new(150, 10),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );

    let _ = font_mid.render_aligned(
        format_args!("{}", data.mode.label()),
        Point::new(w - 2, 4),
        VerticalPosition::Top,
        HorizontalAlignment::Right,
        FontColor::Transparent(Black),
        display,
    );

    // ---- 图表区域 ----
    let chart = Rectangle::new(
        Point::new(left_pad, header_h),
        Size::new((w - left_pad - 2) as u32, (h - header_h - bottom_h) as u32),
    );
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Black)
        .stroke_width(1)
        .stroke_alignment(StrokeAlignment::Inside)
        .build();
    let _ = chart.into_styled(border).draw(display);

    let inner = Rectangle::new(
        Point::new(chart.top_left.x + 2, chart.top_left.y + 2),
        Size::new(
            chart.size.width.saturating_sub(4),
            chart.size.height.saturating_sub(4),
        ),
    );

    match data.mode {
        ChartMode::Minute | ChartMode::Line => {
            if !sd.klines.is_empty() {
                let mut prices: heapless::Vec<f32, KLINE_CAP> = heapless::Vec::new();
                for k in &sd.klines {
                    prices.push(k.close).ok();
                }
                let _ = draw_line(display, inner, &prices);
            }
        }
        _ => {
            let _ = draw_candles(display, inner, &sd.klines);
        }
    }

    // ---- 左侧价位刻度（最高/最低，与图表内部 padding 一致）----
    if let Some((lo, hi)) = raw_range(data.mode, sd) {
        if hi > lo {
            let pad = (hi - lo) * 0.05;
            let hi_s = fmt_price(hi + pad);
            let lo_s = fmt_price(lo - pad);
            let _ = font_small.render_aligned(
                format_args!("{}", hi_s.as_str()),
                Point::new(left_pad - 2, header_h + 4),
                VerticalPosition::Top,
                HorizontalAlignment::Right,
                FontColor::Transparent(Black),
                display,
            );
            let _ = font_small.render_aligned(
                format_args!("{}", lo_s.as_str()),
                Point::new(left_pad - 2, h - bottom_h - 14),
                VerticalPosition::Top,
                HorizontalAlignment::Right,
                FontColor::Transparent(Black),
                display,
            );
        }
    }

    // ---- 底部日期区间（分时模式无）----
    if !sd.klines.is_empty() && !data.mode.is_minute() {
        if let (Some(first), Some(last)) = (sd.klines.first(), sd.klines.last()) {
            let mut line: heapless::String<28> = heapless::String::new();
            let _ = write!(line, "{} ~ {}", fmt_date(first.date).as_str(), fmt_date(last.date).as_str());
            let _ = font_small.render_aligned(
                format_args!("{}", line.as_str()),
                Point::new(w / 2, h - 2),
                VerticalPosition::Top,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
    }

    Ok(())
}

/// 当前模式的原始价格区间 (lo, hi)；无数据返回 None
fn raw_range(mode: ChartMode, sd: &StockData) -> Option<(f32, f32)> {
    if sd.klines.is_empty() {
        return None;
    }
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    if mode.is_line_render() {
        for k in &sd.klines {
            if k.close < lo { lo = k.close; }
            if k.close > hi { hi = k.close; }
        }
    } else {
        for k in &sd.klines {
            if k.low < lo { lo = k.low; }
            if k.high > hi { hi = k.high; }
        }
    }
    Some((lo, hi))
}

/// 20260715 -> "2026-07-15"
fn fmt_date(yyyymmdd: u64) -> heapless::String<12> {
    let mut s: heapless::String<12> = heapless::String::new();
    let y = yyyymmdd / 10000;
    let md = yyyymmdd % 10000;
    let m = md / 100;
    let d = md % 100;
    let _ = write!(s, "{}-{:02}-{:02}", y, m, d);
    s
}

pub fn sleep_renderer(display: &mut EpdDisplay) {
    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();
    let bb = display.bounding_box();
    let cx = bb.top_left.x as i32 + bb.size.width as i32 / 2;
    let cy = bb.top_left.y as i32 + bb.size.height as i32 / 2;
    let _ = font.render_aligned(
        format_args!("股票"),
        Point::new(cx, cy),
        VerticalPosition::Top,
        HorizontalAlignment::Center,
        FontColor::Transparent(Black),
        display,
    );
}
