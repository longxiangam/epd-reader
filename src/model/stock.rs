use alloc::boxed::Box;
use esp_println::println;
use heapless::String;

/// 默认关注的股票（沪深代码，如 sh600519 / sz000001）
pub const DEFAULT_STOCK: &str = "sh600519";

/// K 线容量上限（栈上固定容量，避免堆分配/碎片化）。
/// 覆盖日60 / 周52 / 月24 / 分时48。
pub const KLINE_CAP: usize = 64;

#[derive(Clone, Copy, Default, Debug)]
pub struct KLine {
    /// 日期数字。日/周/月K 为 YYYYMMDD；分时为 YYYYMMDDHHMM（仅用于显示）
    pub date: u64,
    pub open: f32,
    pub close: f32,
    pub high: f32,
    pub low: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChartMode {
    Minute,
    Day,
    Week,
    Month,
    Line,
}

impl ChartMode {
    pub fn next(self) -> Self {
        match self {
            ChartMode::Minute => ChartMode::Day,
            ChartMode::Day => ChartMode::Week,
            ChartMode::Week => ChartMode::Month,
            ChartMode::Month => ChartMode::Line,
            ChartMode::Line => ChartMode::Minute,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ChartMode::Minute => "分时",
            ChartMode::Day => "日K",
            ChartMode::Week => "周K",
            ChartMode::Month => "月K",
            ChartMode::Line => "折线",
        }
    }
    pub fn is_minute(self) -> bool {
        matches!(self, ChartMode::Minute)
    }
    pub fn is_line_render(self) -> bool {
        matches!(self, ChartMode::Minute | ChartMode::Line)
    }
    /// 数据来源；日K 与折线共用 scale=240，切换时可不重新请求
    pub fn source(self) -> ChartSource {
        match self {
            ChartMode::Minute => ChartSource::Minute,
            ChartMode::Day | ChartMode::Line => ChartSource::Day,
            ChartMode::Week => ChartSource::Week,
            ChartMode::Month => ChartSource::Month,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChartSource {
    Minute,
    Day,
    Week,
    Month,
}

pub struct StockData {
    pub mode: ChartMode,
    pub klines: heapless::Vec<KLine, KLINE_CAP>,
    pub last_price: f32,
    pub preclose: f32,
    pub change: f32,
    pub change_pct: f32,
    pub code: String<10>,
}

/// 新浪 K 线接口：明文 HTTP，返回扁平 JSON 数组
/// [{"day":"2026-07-13","open":"...","high":"...","low":"...","close":"...","volume":"..."}]
/// scale：分时=5, 日=240, 周=1200, 月=7200
pub fn build_url(code: &str, mode: ChartMode, count: usize) -> heapless::String<160> {
    let scale: u32 = match mode {
        ChartMode::Minute => 5,
        ChartMode::Day | ChartMode::Line => 240,
        ChartMode::Week => 1200,
        ChartMode::Month => 7200,
    };
    let mut s: heapless::String<160> = heapless::String::new();
    let _ = s.push_str("http://money.finance.sina.com.cn/quotes_service/api/json_v2.php/CN_MarketData.getKLineData?symbol=");
    let _ = s.push_str(code);
    let _ = s.push_str("&scale=");
    push_u32(&mut s, scale);
    let _ = s.push_str("&ma=no&datalen=");
    push_u32(&mut s, count as u32);
    s
}

pub fn bar_count(mode: ChartMode) -> usize {
    match mode {
        ChartMode::Minute => 48,        // 一个交易日约 48 个 5 分钟点
        ChartMode::Day | ChartMode::Line => 60,
        ChartMode::Week => 52,          // 约 1 年
        ChartMode::Month => 24,         // 约 2 年
    }
}

/// no_std 下 f32 不能用 parse / FromStr，手写定点解析（整数 + 小数）
fn parse_f32(s: &str) -> f32 {
    let b = s.as_bytes();
    if b.is_empty() {
        return 0.0;
    }
    let (neg, start) = if b[0] == b'-' { (true, 1) } else { (false, 0) };
    let mut int_v: f32 = 0.0;
    let mut frac_v: f32 = 0.0;
    let mut div: f32 = 1.0;
    let mut in_frac = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if c == b'.' {
            in_frac = true;
        } else if c.is_ascii_digit() {
            let d = (c - b'0') as f32;
            if in_frac {
                div *= 10.0;
                frac_v = frac_v * 10.0 + d;
            } else {
                int_v = int_v * 10.0 + d;
            }
        } else {
            break;
        }
        i += 1;
    }
    let v = int_v + frac_v / div;
    if neg { -v } else { v }
}

fn parse_date(s: &str) -> u64 {
    let mut n: u64 = 0;
    for c in s.bytes() {
        if c.is_ascii_digit() {
            n = n * 10 + (c - b'0') as u64;
        }
    }
    n
}

/// 解析新浪 K 线响应：[{"day":"...","open":"...","high":"...","low":"...","close":"...","volume":"..."},...]
/// 用字节扫描直接提取字段（新浪所有值都是带引号字符串），不依赖 mini_json——
/// 后者会为每个字段分配 alloc::String，60 根 × 6 字段需要 ~12-15KB 堆，在 64KB 堆下会 OOM。
/// 扫描器零堆分配，直接填入 heapless 结构。
pub fn parse_kline(data: &[u8], code: &str, mode: ChartMode) -> Option<Box<StockData>> {
    let s = core::str::from_utf8(data).ok()?;
    let b = s.as_bytes();
    let n = b.len();

    let mut code_s: String<10> = String::new();
    let _ = code_s.push_str(code);

    let mut out = Box::new(StockData {
        mode,
        klines: heapless::Vec::new(),
        last_price: 0.0,
        preclose: 0.0,
        change: 0.0,
        change_pct: 0.0,
        code: code_s,
    });

    let mut i = 0usize;
    let mut cur = KLine::default();
    while i < n {
        // 定位 key 起始引号
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= n {
            break;
        }
        let key_start = i;
        while i < n && b[i] != b'"' {
            i += 1;
        }
        let key = &b[key_start..i];
        if i < n {
            i += 1;
        }
        // 跳到 value 起始引号（值都是带引号字符串）
        while i < n && b[i] != b'"' {
            i += 1;
        }
        if i >= n {
            break;
        }
        i += 1;
        let val_start = i;
        while i < n && b[i] != b'"' {
            i += 1;
        }
        let val = core::str::from_utf8(&b[val_start..i]).unwrap_or("");
        if i < n {
            i += 1;
        }

        if key == b"open" {
            cur.open = parse_f32(val);
        } else if key == b"high" {
            cur.high = parse_f32(val);
        } else if key == b"low" {
            cur.low = parse_f32(val);
        } else if key == b"close" {
            cur.close = parse_f32(val);
        } else if key == b"day" {
            cur.date = parse_date(val);
        }

        // 一个对象结束（最后一个字段后是 '}'）
        let mut j = i;
        while j < n && b[j] == b' ' {
            j += 1;
        }
        if j < n && b[j] == b'}' {
            let _ = out.klines.push(cur);
            cur = KLine::default();
            i = j + 1;
        }
    }

    // 现价 / 昨收：从最后两根推。分时用首根 open 作今日开盘。
    let kn = out.klines.len();
    let (last_price, preclose) = if kn >= 2 {
        if mode.is_minute() {
            (out.klines[kn - 1].close, out.klines[0].open)
        } else {
            (out.klines[kn - 1].close, out.klines[kn - 2].close)
        }
    } else if kn == 1 {
        (out.klines[0].close, out.klines[0].open)
    } else {
        (0.0, 0.0)
    };
    out.last_price = last_price;
    out.preclose = preclose;
    out.change = last_price - preclose;
    out.change_pct = if preclose > 0.0 { out.change / preclose * 100.0 } else { 0.0 };

    println!("stock parsed: mode={:?} rows={} price={}", mode, out.klines.len(), last_price);
    Some(out)
}

fn push_u32<const N: usize>(s: &mut String<N>, mut n: u32) {
    if n == 0 {
        let _ = s.push('0');
        return;
    }
    let mut digits = [0u8; 10];
    let mut pos = 10;
    while n > 0 {
        pos -= 1;
        digits[pos] = (n % 10) as u8 + b'0';
        n /= 10;
    }
    for &d in &digits[pos..] {
        let _ = s.push(d as char);
    }
}

/// 2 位小数，不带符号
pub fn fmt_price(v: f32) -> String<12> {
    let neg = v < 0.0;
    let abs = if neg { -v } else { v };
    let scaled = (abs * 100.0 + 0.5) as u32;
    let int_part = scaled / 100;
    let frac = scaled % 100;
    let mut s: String<12> = String::new();
    if neg {
        let _ = s.push('-');
    }
    push_u32(&mut s, int_part);
    let _ = s.push('.');
    let _ = s.push((b'0' + (frac / 10) as u8) as char);
    let _ = s.push((b'0' + (frac % 10) as u8) as char);
    s
}

/// 2 位小数，带正负号（用于涨跌额 / 涨跌幅）
pub fn fmt_signed(v: f32) -> String<16> {
    let neg = v < 0.0;
    let abs = if neg { -v } else { v };
    let scaled = (abs * 100.0 + 0.5) as u32;
    let int_part = scaled / 100;
    let frac = scaled % 100;
    let mut s: String<16> = String::new();
    let _ = s.push(if neg { '-' } else { '+' });
    push_u32(&mut s, int_part);
    let _ = s.push('.');
    let _ = s.push((b'0' + (frac / 10) as u8) as char);
    let _ = s.push((b'0' + (frac % 10) as u8) as char);
    s
}
