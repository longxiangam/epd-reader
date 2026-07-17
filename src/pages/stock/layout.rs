use core::fmt::Write;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Point, Primitive, Size};
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::Drawable;
use epd_waveshare::color::Black;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::render_data::StockRenderData;
use crate::display::EpdDisplay;
use crate::model::stock::{fmt_price, fmt_signed, ChartMode, KLINE_CAP, RealtimeQuote, StockData};
use crate::widgets::kline::{draw_candles, draw_line, map_y, padded_range, visible_count};

pub fn draw<D>(display: &mut D, data: &StockRenderData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let w = data.w;
    let h = data.h;

    let font_mid: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();
    let font_small: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>();

    // 状态栏（右上角：加载/wifi/电量，与天气页一致）
    let cy: i32 = 11;
    if data.request_loading {
        crate::widgets::draw_icon::draw_loading_icon(Point::new(w - 118, cy), display);
    }
    crate::widgets::draw_icon::draw_wifi_status(Point::new(w - 100, cy), display);
    if let Some(percent) = data.battery_percent {
        let _ = crate::widgets::battery::draw_battery(percent, Point::new(w - 60, 4), Black, &font_small, display);
    }

    match data.data {
        None => {
            // 加载中：不显示文字，只靠右上角加载图标
            // 非加载（拉取失败/未配置）：显示错误信息
            if !data.loading {
                let msg = data.err_msg.unwrap_or("无数据 按1刷新");
                let _ = font_mid.render_aligned(
                    format_args!("{}", msg),
                    Point::new(w / 2, h / 2),
                    VerticalPosition::Top,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                );
            }
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
    let header_h: i32 = 32;
    let bottom_h: i32 = 16;
    let left_pad: i32 = 52; // 左侧留白画价位刻度（容下 2 位小数价位，如 "1256.60"）

    // ---- 第一行：名称 + 代码 | 模式 ----
    let mut title: heapless::String<44> = heapless::String::new();
    if !sd.name.is_empty() {
        let _ = title.push_str(sd.name.as_str());
        let _ = title.push_str(" ");
    }
    let _ = title.push_str(sd.code.as_str());
    let _ = font_mid.render_aligned(
        format_args!("{}", title.as_str()),
        Point::new(2, 2),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );
    let _ = font_mid.render_aligned(
        format_args!("{}", data.mode.label()),
        Point::new(w / 2, 2),
        VerticalPosition::Top,
        HorizontalAlignment::Center,
        FontColor::Transparent(Black),
        display,
    );

    // ---- 第二行：现价 | 涨跌额 涨跌幅% ----
    let price = fmt_price(sd.last_price);
    let _ = font_small.render_aligned(
        format_args!("{}", price.as_str()),
        Point::new(2, 19),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );
    let mut chg_line: heapless::String<28> = heapless::String::new();
    let _ = chg_line.push_str(fmt_signed(sd.change).as_str());
    let _ = chg_line.push_str("  ");
    let _ = chg_line.push_str(fmt_signed(sd.change_pct).as_str());
    let _ = chg_line.push_str("%");
    let _ = font_small.render_aligned(
        format_args!("{}", chg_line.as_str()),
        Point::new(w - 2, 20),
        VerticalPosition::Top,
        HorizontalAlignment::Right,
        FontColor::Transparent(Black),
        display,
    );

    // 行情模式：画盘口表，不走图表
    if data.mode == ChartMode::Quote {
        if let Some(ref q) = sd.quote {
            return draw_quote(display, q, w, h, font_mid);
        }
    }

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

    // 分时：跨天边界 (index, x)（竖虚线位置 + 底部时间防重叠共用）
    let boundary: Option<(usize, i32)> = if data.mode.is_minute() && sd.klines.len() >= 2 {
        let kn = sd.klines.len();
        let iw = inner.size.width as i32;
        (1..kn).find_map(|idx| {
            if sd.klines[idx - 1].date / 1_000_000 != sd.klines[idx].date / 1_000_000 {
                let x = inner.top_left.x + (iw as f32 * idx as f32 / (kn as f32 - 1.0)) as i32;
                Some((idx, x))
            } else {
                None
            }
        })
    } else {
        None
    };

    match data.mode {
        ChartMode::Minute => {
            if sd.klines.len() >= 2 {
                let mut prices: heapless::Vec<f32, KLINE_CAP> = heapless::Vec::new();
                for k in &sd.klines {
                    prices.push(k.close).ok();
                }
                let _ = draw_line(display, inner, &prices);

                // 最高/最低/当前 价格参考线
                let mut lo = f32::MAX;
                let mut hi = f32::MIN;
                for &p in &prices {
                    if p < lo { lo = p; }
                    if p > hi { hi = p; }
                }
                let cur = *prices.last().unwrap_or(&0.0);
                let (plo, phi) = padded_range(lo, hi);
                let itop = inner.top_left.y;
                let ih = inner.size.height as i32;
                let ix0 = inner.top_left.x;
                let iw = inner.size.width as i32;
                let y_high = map_y(hi, plo, phi, itop, ih);
                let y_low = map_y(lo, plo, phi, itop, ih);
                let y_cur = map_y(cur, plo, phi, itop, ih);
                // 当前价标签 y：避免与最高/最低重叠（高价保持在上、低价在下，当前避让）
                let mut y_cur_lbl = y_cur;
                if y_cur - y_high < 12 { y_cur_lbl = y_high + 12; }
                if y_low - y_cur_lbl < 12 { y_cur_lbl = y_low - 12; }
                // 最高/最低 横虚线
                let _ = draw_dashed_hline(display, ix0, y_high, iw);
                let _ = draw_dashed_hline(display, ix0, y_low, iw);
                // 左侧价格标签（高/低/当前）
                let _ = font_small.render_aligned(
                    format_args!("{}", fmt_price(hi).as_str()),
                    Point::new(left_pad - 2, y_high - 6),
                    VerticalPosition::Top, HorizontalAlignment::Right,
                    FontColor::Transparent(Black), display,
                );
                let _ = font_small.render_aligned(
                    format_args!("{}", fmt_price(lo).as_str()),
                    Point::new(left_pad - 2, y_low - 6),
                    VerticalPosition::Top, HorizontalAlignment::Right,
                    FontColor::Transparent(Black), display,
                );
                let _ = font_small.render_aligned(
                    format_args!("{}", fmt_price(cur).as_str()),
                    Point::new(left_pad - 2, y_cur_lbl - 6),
                    VerticalPosition::Top, HorizontalAlignment::Right,
                    FontColor::Transparent(Black), display,
                );
                // 当前点小圆点（折线末端）
                let dot = PrimitiveStyleBuilder::new().fill_color(Black).build();
                let _ = Circle::new(Point::new(ix0 + iw - 3, y_cur - 3), 6)
                    .into_styled(dot).draw(display);
                // 跨天竖虚线
                if let Some((_, bx)) = boundary {
                    let _ = draw_dashed_vline(display, bx, itop, ih);
                }
            }
        }
        ChartMode::Line => {
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

    // ---- 左侧价位刻度（最高/最低；分时已在图表内画了参考线，跳过）----
    if !data.mode.is_minute() {
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
    }

    // ---- 底部坐标 ----
    if !sd.klines.is_empty() {
        let total = sd.klines.len();
        let by = h - bottom_h + 1;
        if data.mode.is_minute() {
            // 分时：左=最早时间，右=最晚时间，中间=跨天虚线时间（间距太近则隐藏避免重叠）
            let first = &sd.klines[0];
            let last = &sd.klines[total - 1];
            let left_x = inner.top_left.x;
            let right_x = inner.top_left.x + inner.size.width as i32;
            let gap = 92; // 约一个标签宽度 + 间距
            let show_left = boundary.map_or(true, |(_, bx)| bx - left_x >= gap);
            let show_boundary = boundary.map_or(false, |(_, bx)| right_x - bx >= gap);
            let cross_day = boundary.is_some();
            if show_left {
                // 跨天才带日期；同一天只显示时间
                let txt = if cross_day { fmt_md_hm(first.date) } else { fmt_hm(first.date) };
                let _ = font_small.render_aligned(
                    format_args!("{}", txt.as_str()),
                    Point::new(left_x, by),
                    VerticalPosition::Top, HorizontalAlignment::Left,
                    FontColor::Transparent(Black), display,
                );
            }
            if show_boundary {
                if let Some((bidx, bx)) = boundary {
                    let _ = font_small.render_aligned(
                        format_args!("{}", fmt_md_hm(sd.klines[bidx].date).as_str()),
                        Point::new(bx, by),
                        VerticalPosition::Top, HorizontalAlignment::Center,
                        FontColor::Transparent(Black), display,
                    );
                }
            }
            // 右：当前时间（只显示时间，与虚线同一天，无需日期）
            let _ = font_small.render_aligned(
                format_args!("{}", fmt_hm(last.date).as_str()),
                Point::new(right_x, by),
                VerticalPosition::Top, HorizontalAlignment::Right,
                FontColor::Transparent(Black), display,
            );
        } else {
            // K 线/折线：日期区间（蜡烛模式按可见窗口）
            let inner_w = inner.size.width as i32;
            let start = if data.mode.is_line_render() {
                0
            } else {
                total - visible_count(inner_w, total)
            };
            let first = &sd.klines[start];
            let last = &sd.klines[total - 1];
            let mut line: heapless::String<28> = heapless::String::new();
            let _ = write!(line, "{} ~ {}", fmt_date(first.date).as_str(), fmt_date(last.date).as_str());
            let _ = font_small.render_aligned(
                format_args!("{}", line.as_str()),
                Point::new(w / 2, by),
                VerticalPosition::Top,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
    }

    Ok(())
}

/// 从分钟数据日期(YYYYMMDDHHMMSS 数字串)提取 "MM-DD HH:MM"
fn fmt_md_hm(date: u64) -> heapless::String<14> {
    let md = (date / 1_000_000) % 10000;
    let mo = md / 100;
    let dy = md % 100;
    let hhmm = (date / 100) % 10000;
    let hh = hhmm / 100;
    let mm = hhmm % 100;
    let mut s: heapless::String<14> = heapless::String::new();
    let _ = write!(s, "{:02}-{:02} {:02}:{:02}", mo, dy, hh, mm);
    s
}

/// 只显示 "HH:MM"（同一天内用）
fn fmt_hm(date: u64) -> heapless::String<14> {
    let hhmm = (date / 100) % 10000;
    let mut s: heapless::String<14> = heapless::String::new();
    let _ = write!(s, "{:02}:{:02}", hhmm / 100, hhmm % 100);
    s
}

/// 竖虚线（跨天分隔标记）
fn draw_dashed_vline<D>(display: &mut D, x: i32, top: i32, height: i32) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = PrimitiveStyleBuilder::new().stroke_color(Black).stroke_width(1).build();
    let dash = 3;
    let gap = 2;
    let mut y = top;
    while y < top + height {
        let y2 = (y + dash).min(top + height);
        Line::new(Point::new(x, y), Point::new(x, y2))
            .into_styled(style.clone())
            .draw(display)?;
        y += dash + gap;
    }
    Ok(())
}

/// 横虚线（最高/最低价位参考线）
fn draw_dashed_hline<D>(display: &mut D, x0: i32, y: i32, width: i32) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = PrimitiveStyleBuilder::new().stroke_color(Black).stroke_width(1).build();
    let dash = 3;
    let gap = 2;
    let mut x = x0;
    while x < x0 + width {
        let x2 = (x + dash).min(x0 + width);
        Line::new(Point::new(x, y), Point::new(x2, y))
            .into_styled(style.clone())
            .draw(display)?;
        x += dash + gap;
    }
    Ok(())
}

/// 行情/盘口视图（表格布局，16px 字体）
fn draw_quote<D>(display: &mut D, q: &RealtimeQuote, w: i32, h: i32, font: &FontRenderer) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let top = 34;
    let bottom = h - 18;
    let mid_x = w / 2;
    let row_h: i32 = (bottom - top - 12) / 11;
    let y0 = top + 6;

    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Black).stroke_width(1).stroke_alignment(StrokeAlignment::Inside).build();

    // 外框
    let _ = Rectangle::new(Point::new(0, top), Size::new(w as u32, (bottom - top) as u32))
        .into_styled(border.clone()).draw(display);
    // 列分隔线
    let _ = Line::new(Point::new(mid_x, top), Point::new(mid_x, bottom))
        .into_styled(border.clone()).draw(display);

    // 左列：基本信息（标签左对齐 + 值右对齐）
    let info: [(&str, f32); 11] = [
        ("今开", q.open), ("昨收", q.preclose),
        ("最高", q.high), ("最低", q.low),
        ("换手%", q.turnover), ("振幅%", q.amplitude),
        ("量(手)", q.volume), ("额(万)", q.amount),
        ("市盈", q.pe), ("市净", q.pb), ("总市值", q.total_mkt),
    ];
    for (i, &(label, val)) in info.iter().enumerate() {
        let yy = y0 + i as i32 * row_h;
        let _ = font.render_aligned(
            format_args!("{}", label),
            Point::new(4, yy), VerticalPosition::Top, HorizontalAlignment::Left,
            FontColor::Transparent(Black), display,
        );
        let _ = font.render_aligned(
            format_args!("{}", fmt_price(val).as_str()),
            Point::new(mid_x - 4, yy), VerticalPosition::Top, HorizontalAlignment::Right,
            FontColor::Transparent(Black), display,
        );
    }

    // 右列：买卖5档（卖5在上 → 现价 → 买5在下）
    let sell_end = y0 + 5 * row_h;
    for i in 0..5usize {
        let yy = y0 + i as i32 * row_h;
        let lvl = &q.sells[4 - i];
        let mut label_price: heapless::String<24> = heapless::String::new();
        let _ = write!(label_price, "卖{} {}", 5 - i, fmt_price(lvl.price).as_str());
        let _ = font.render_aligned(
            format_args!("{}", label_price.as_str()),
            Point::new(mid_x + 4, yy), VerticalPosition::Top, HorizontalAlignment::Left,
            FontColor::Transparent(Black), display,
        );
        let _ = font.render_aligned(
            format_args!("{}", lvl.vol),
            Point::new(w - 4, yy), VerticalPosition::Top, HorizontalAlignment::Right,
            FontColor::Transparent(Black), display,
        );
    }
    // 卖/现价 分隔线
    let _ = Line::new(Point::new(mid_x, sell_end), Point::new(w, sell_end))
        .into_styled(border.clone()).draw(display);
    // 现价行
    let _ = font.render_aligned(
        format_args!("-> {}", fmt_price(q.price).as_str()),
        Point::new(mid_x + 4, sell_end + 3), VerticalPosition::Top, HorizontalAlignment::Left,
        FontColor::Transparent(Black), display,
    );
    let price_end = sell_end + row_h;
    // 现价/买 分隔线
    let _ = Line::new(Point::new(mid_x, price_end), Point::new(w, price_end))
        .into_styled(border.clone()).draw(display);
    // 买1..买5
    for i in 0..5usize {
        let yy = price_end + 3 + i as i32 * row_h;
        let lvl = &q.buys[i];
        let mut label_price: heapless::String<24> = heapless::String::new();
        let _ = write!(label_price, "买{} {}", i + 1, fmt_price(lvl.price).as_str());
        let _ = font.render_aligned(
            format_args!("{}", label_price.as_str()),
            Point::new(mid_x + 4, yy), VerticalPosition::Top, HorizontalAlignment::Left,
            FontColor::Transparent(Black), display,
        );
        let _ = font.render_aligned(
            format_args!("{}", lvl.vol),
            Point::new(w - 4, yy), VerticalPosition::Top, HorizontalAlignment::Right,
            FontColor::Transparent(Black), display,
        );
    }

    // 底部：格式化时间
    let dt = q.datetime.as_str();
    let mut buf: heapless::String<24> = heapless::String::new();
    if dt.len() >= 12 {
        let _ = write!(buf, "{}-{}-{} {}:{}",
            &dt[..4], &dt[4..6], &dt[6..8], &dt[8..10], &dt[10..12]);
    } else {
        let _ = buf.push_str(dt);
    }
    let _ = font.render_aligned(
        format_args!("{}", buf.as_str()),
        Point::new(w / 2, h - 16), VerticalPosition::Top, HorizontalAlignment::Center,
        FontColor::Transparent(Black), display,
    );

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
    let w = display.bounding_box().size.width as i32;
    let cy: i32 = 11;
    if crate::wifi::is_request_loading() {
        crate::widgets::draw_icon::draw_loading_icon(Point::new(w - 118, cy), display);
    }
    crate::widgets::draw_icon::draw_wifi_status(Point::new(w - 100, cy), display);
    crate::widgets::draw_icon::draw_moon_icon(Point::new(w - 82, 4), display);
}
