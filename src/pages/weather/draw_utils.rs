use time::Weekday;

pub fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "周一",
        Weekday::Tuesday => "周二",
        Weekday::Wednesday => "周三",
        Weekday::Thursday => "周四",
        Weekday::Friday => "周五",
        Weekday::Saturday => "周六",
        Weekday::Sunday => "周日",
    }
}
