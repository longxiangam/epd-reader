use embedded_graphics::Drawable;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::image::Image;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{PixelColor, Point};
use tinybmp::Bmp;

/// 天气类型，由心知天气 code_day / code_night 映射而来
/// 对照表: https://docs.seniverse.com/api/start/code.html
#[derive(Clone, Copy, Debug)]
pub enum WeatherKind {
    Sunny,          // 0,1,2,3 晴
    PartlyCloudy,   // 4 多云
    MostlyCloudy,   // 5,6 晴间多云
    Cloudy,         // 7,8 大部多云
    Overcast,       // 9 阴
    LightRain,      // 10 阵雨, 13 小雨
    ModerateRain,   // 14 中雨
    HeavyRain,      // 15,16,17,18 大雨~特大暴雨
    Thunderstorm,   // 11,12 雷阵雨
    Sleet,          // 19 冻雨, 20 雨夹雪
    LightSnow,      // 21 阵雪, 22 小雪
    ModerateSnow,   // 23 中雪
    HeavySnow,      // 24,25 大雪, 暴雪
    Dust,           // 26,27,28,29 浮尘~强沙尘暴
    Fog,            // 30 雾
    Haze,           // 31 霸
    Wind,           // 32,33,34,35,36 风~龙卷风
    Cold,           // 37 冷
    Hot,            // 38 热
    Unknown,        // 99 未知
}

impl WeatherKind {
    pub fn from_code(code: &str) -> Self {
        match code {
            "0" | "1" | "2" | "3" => WeatherKind::Sunny,
            "4" => WeatherKind::PartlyCloudy,
            "5" | "6" => WeatherKind::MostlyCloudy,
            "7" | "8" => WeatherKind::Cloudy,
            "9" => WeatherKind::Overcast,
            "10" | "13" => WeatherKind::LightRain,
            "14" => WeatherKind::ModerateRain,
            "15" | "16" | "17" | "18" => WeatherKind::HeavyRain,
            "11" | "12" => WeatherKind::Thunderstorm,
            "19" | "20" => WeatherKind::Sleet,
            "21" | "22" => WeatherKind::LightSnow,
            "23" => WeatherKind::ModerateSnow,
            "24" | "25" => WeatherKind::HeavySnow,
            "26" | "27" | "28" | "29" => WeatherKind::Dust,
            "30" => WeatherKind::Fog,
            "31" => WeatherKind::Haze,
            "32" | "33" | "34" | "35" | "36" => WeatherKind::Wind,
            "37" => WeatherKind::Cold,
            "38" => WeatherKind::Hot,
            _ => WeatherKind::Unknown,
        }
    }
}

macro_rules! load_bmp {
    ($name:literal) => {
        Bmp::<BinaryColor>::from_slice(include_bytes!(concat!("../../icons/weather/", $name, ".bmp")))
            .unwrap()
    };
}

fn get_bmp(kind: WeatherKind) -> Bmp<'static, BinaryColor> {
    match kind {
        WeatherKind::Sunny => load_bmp!("sunny"),
        WeatherKind::PartlyCloudy => load_bmp!("partly_cloudy"),
        WeatherKind::MostlyCloudy => load_bmp!("mostly_cloudy"),
        WeatherKind::Cloudy => load_bmp!("cloudy"),
        WeatherKind::Overcast => load_bmp!("overcast"),
        WeatherKind::LightRain => load_bmp!("light_rain"),
        WeatherKind::ModerateRain => load_bmp!("moderate_rain"),
        WeatherKind::HeavyRain => load_bmp!("heavy_rain"),
        WeatherKind::Thunderstorm => load_bmp!("thunderstorm"),
        WeatherKind::Sleet => load_bmp!("sleet"),
        WeatherKind::LightSnow => load_bmp!("light_snow"),
        WeatherKind::ModerateSnow => load_bmp!("moderate_snow"),
        WeatherKind::HeavySnow => load_bmp!("heavy_snow"),
        WeatherKind::Dust => load_bmp!("dust"),
        WeatherKind::Fog => load_bmp!("fog"),
        WeatherKind::Haze => load_bmp!("haze"),
        WeatherKind::Wind => load_bmp!("wind"),
        WeatherKind::Cold => load_bmp!("cold"),
        WeatherKind::Hot => load_bmp!("hot"),
        WeatherKind::Unknown => load_bmp!("unknown"),
    }
}

/// 在指定中心位置绘制天气图标（32x32 BMP 位图）
pub fn draw_weather_icon<C, D>(
    kind: WeatherKind,
    center: Point,
    _icon_size: u32,
    _color: C,
    target: &mut D,
) -> Result<(), D::Error>
where
    C: PixelColor + Clone,
    D: DrawTarget<Color = BinaryColor>,
{
    let bmp = get_bmp(kind);
    let top_left = Point::new(center.x - 16, center.y - 16);
    Image::new(&bmp, top_left).draw(target)?;
    Ok(())
}
