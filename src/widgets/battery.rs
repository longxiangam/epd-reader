use embedded_graphics::Drawable;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::PixelColor;
use embedded_graphics::prelude::{Point, Primitive, Size};
use embedded_graphics::primitives::{Rectangle, PrimitiveStyleBuilder};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

const BODY_W: u32 = 22;
const BODY_H: u32 = 12;
const NUB_W: u32 = 2;
const NUB_H: u32 = 5;
const PAD: u32 = 2;

pub fn draw_battery<C, D>(
    percent: u32,
    position: Point,
    color: C,
    font: &FontRenderer,
    target: &mut D,
) -> Result<(), D::Error>
where
    C: PixelColor + Clone,
    D: DrawTarget<Color = C>,
{
    let outline = PrimitiveStyleBuilder::new()
        .stroke_color(color.clone())
        .stroke_width(1)
        .build();

    let fill_style = PrimitiveStyleBuilder::new()
        .fill_color(color.clone())
        .build();

    // Battery body outline
    Rectangle::new(position, Size::new(BODY_W, BODY_H))
        .into_styled(outline.clone())
        .draw(target)?;

    // Terminal nub on right side
    let nub_x = position.x + BODY_W as i32;
    let nub_y = position.y + (BODY_H - NUB_H) as i32 / 2;
    Rectangle::new(Point::new(nub_x, nub_y), Size::new(NUB_W, NUB_H))
        .into_styled(fill_style.clone())
        .draw(target)?;

    // Fill bar (proportional to percent, 0-100)
    let inner_w = BODY_W.saturating_sub(PAD * 2);
    let inner_h = BODY_H.saturating_sub(PAD * 2);
    if inner_w > 0 && inner_h > 0 && percent > 0 {
        let fill_w = (inner_w * percent.min(100)) / 100;
        if fill_w > 0 {
            Rectangle::new(
                Point::new(position.x + PAD as i32, position.y + PAD as i32),
                Size::new(fill_w, inner_h),
            )
            .into_styled(fill_style)
            .draw(target)?;
        }
    }

    // Percentage text to the right of the icon
    let text_x = position.x + BODY_W as i32 + NUB_W as i32 + 3;
    let text_y = position.y + BODY_H as i32 / 2;
    let _ = font.render_aligned(
        format_args!("{}%", percent),
        Point::new(text_x, text_y),
        VerticalPosition::Center,
        HorizontalAlignment::Left,
        FontColor::Transparent(color),
        target,
    );

    Ok(())
}
