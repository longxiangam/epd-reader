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
    Quote,
}

impl ChartMode {
    pub fn next(self) -> Self {
        match self {
            ChartMode::Minute => ChartMode::Day,
            ChartMode::Day => ChartMode::Week,
            ChartMode::Week => ChartMode::Month,
            ChartMode::Month => ChartMode::Line,
            ChartMode::Line => ChartMode::Quote,
            ChartMode::Quote => ChartMode::Minute,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            ChartMode::Minute => ChartMode::Quote,
            ChartMode::Quote => ChartMode::Line,
            ChartMode::Line => ChartMode::Month,
            ChartMode::Month => ChartMode::Week,
            ChartMode::Week => ChartMode::Day,
            ChartMode::Day => ChartMode::Minute,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ChartMode::Minute => "分时",
            ChartMode::Day => "日K",
            ChartMode::Week => "周K",
            ChartMode::Month => "月K",
            ChartMode::Line => "折线",
            ChartMode::Quote => "行情",
        }
    }
    pub fn is_minute(self) -> bool {
        matches!(self, ChartMode::Minute)
    }
    /// 实时模式（分时/行情）：每 2 分钟唤醒刷新
    pub fn is_realtime(self) -> bool {
        matches!(self, ChartMode::Minute | ChartMode::Quote)
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
            ChartMode::Quote => ChartSource::Quote,
        }
    }
    /// 编码为 u8 用于存 rtc_fast（跨深睡重启保留模式）
    pub fn encode(self) -> u8 {
        match self {
            ChartMode::Day => 0,
            ChartMode::Week => 1,
            ChartMode::Month => 2,
            ChartMode::Line => 3,
            ChartMode::Minute => 4,
            ChartMode::Quote => 5,
        }
    }
    pub fn decode(v: u8) -> Self {
        match v {
            1 => ChartMode::Week,
            2 => ChartMode::Month,
            3 => ChartMode::Line,
            4 => ChartMode::Minute,
            5 => ChartMode::Quote,
            _ => ChartMode::Day,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChartSource {
    Minute,
    Day,
    Week,
    Month,
    Quote,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct QuoteLevel {
    pub price: f32,
    pub vol: u32,
}

#[derive(Default, Debug)]
pub struct RealtimeQuote {
    pub open: f32,
    pub preclose: f32,
    pub price: f32,
    pub high: f32,
    pub low: f32,
    pub volume: f32,      // 成交量(手)
    pub amount: f32,      // 成交额(万元)
    pub turnover: f32,    // 换手率%
    pub amplitude: f32,   // 振幅%
    pub pe: f32,          // 市盈率
    pub pb: f32,          // 市净率
    pub total_mkt: f32,   // 总市值(亿)
    pub circ_mkt: f32,    // 流通市值(亿)
    pub buys: [QuoteLevel; 5],
    pub sells: [QuoteLevel; 5],
    pub datetime: String<15>, // "20260717113859"
}

pub struct StockData {
    pub mode: ChartMode,
    pub klines: heapless::Vec<KLine, KLINE_CAP>,
    pub last_price: f32,
    pub preclose: f32,
    pub change: f32,
    pub change_pct: f32,
    pub code: String<10>,
    pub name: String<32>,
    pub quote: Option<Box<RealtimeQuote>>,
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
        ChartMode::Quote => 0,
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
        ChartMode::Quote => 0,          // 行情模式不取 K 线
    }
}

/// 腾讯实时行情接口（明文 HTTP，无需 Referer，含换手率/振幅/总市值等全部字段）
pub fn build_quote_url(code: &str) -> heapless::String<64> {
    let mut s: heapless::String<64> = heapless::String::new();
    let _ = s.push_str("http://qt.gtimg.cn/q=");
    let _ = s.push_str(code);
    s
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
pub fn parse_kline(data: &[u8], code: &str, name: &str, mode: ChartMode) -> Option<Box<StockData>> {
    let s = core::str::from_utf8(data).ok()?;
    let b = s.as_bytes();
    let n = b.len();

    let mut code_s: String<10> = String::new();
    let _ = code_s.push_str(code);
    let mut name_s: String<32> = String::new();
    let _ = name_s.push_str(name);

    let mut out = Box::new(StockData {
        mode,
        klines: heapless::Vec::new(),
        last_price: 0.0,
        preclose: 0.0,
        change: 0.0,
        change_pct: 0.0,
        code: code_s,
        name: name_s,
        quote: None,
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

    // 按日期升序排序，确保时间顺序正确（避免接口返回顺序导致首尾颠倒）
    {
        let m = out.klines.len();
        for a in 1..m {
            let mut bidx = a;
            while bidx > 0 && out.klines[bidx - 1].date > out.klines[bidx].date {
                out.klines.swap(bidx - 1, bidx);
                bidx -= 1;
            }
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

fn parse_u32(s: &str) -> u32 {
    let mut n: u32 = 0;
    for c in s.bytes() {
        if c.is_ascii_digit() {
            n = n * 10 + (c - b'0') as u32;
        } else {
            break;
        }
    }
    n
}

fn fld_bytes<'a>(fields: &[&'a [u8]], i: usize) -> &'a str {
    core::str::from_utf8(fields.get(i).copied().unwrap_or(b"0")).unwrap_or("0")
}

/// GBK 安全的 ~ 分割：lead byte(0x81-0xFE) 跳过下一字节，
/// 避免把 GBK 尾字节 0x7E(~) 误判为分隔符
fn split_gbk_tilde(data: &[u8]) -> heapless::Vec<&[u8], 80> {
    let mut result: heapless::Vec<&[u8], 80> = heapless::Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if (0x81..=0xFE).contains(&b) && i + 1 < data.len() {
            i += 2;
            continue;
        }
        if b == b'~' {
            let _ = result.push(&data[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    let _ = result.push(&data[start..]);
    result
}

/// 解析腾讯实时行情 qt.gtimg.cn
pub fn parse_quote(data: &[u8], code: &str, name: &str) -> Option<Box<StockData>> {
    let start = data.iter().position(|&b| b == b'"')? + 1;
    let end = start + data[start..].iter().position(|&b| b == b'"')?;
    let body = &data[start..end];
    let fields = split_gbk_tilde(body);
    if fields.len() < 47 {
        return None;
    }

    let mut buys = [QuoteLevel::default(); 5];
    let mut sells = [QuoteLevel::default(); 5];
    for i in 0..5usize {
        buys[i] = QuoteLevel {
            price: parse_f32(fld_bytes(&fields, 9 + 2 * i)),
            vol: parse_u32(fld_bytes(&fields, 10 + 2 * i)),
        };
        sells[i] = QuoteLevel {
            price: parse_f32(fld_bytes(&fields, 19 + 2 * i)),
            vol: parse_u32(fld_bytes(&fields, 20 + 2 * i)),
        };
    }

    let mut datetime: String<15> = String::new();
    let _ = datetime.push_str(fld_bytes(&fields, 30));

    let price = parse_f32(fld_bytes(&fields, 3));
    let preclose = parse_f32(fld_bytes(&fields, 4));
    let rq = RealtimeQuote {
        open: parse_f32(fld_bytes(&fields, 5)),
        preclose,
        price,
        high: parse_f32(fld_bytes(&fields, 33)),
        low: parse_f32(fld_bytes(&fields, 34)),
        volume: parse_f32(fld_bytes(&fields, 6)),
        amount: parse_f32(fld_bytes(&fields, 37)),
        turnover: parse_f32(fld_bytes(&fields, 38)),
        amplitude: parse_f32(fld_bytes(&fields, 43)),
        pe: parse_f32(fld_bytes(&fields, 39)),
        pb: parse_f32(fld_bytes(&fields, 46)),
        total_mkt: parse_f32(fld_bytes(&fields, 45)),
        circ_mkt: parse_f32(fld_bytes(&fields, 44)),
        buys,
        sells,
        datetime,
    };
    println!("stock quote parsed: price={} high={} low={} turnover={}", price, rq.high, rq.low, rq.turnover);

    let mut code_s: String<10> = String::new();
    let _ = code_s.push_str(code);
    let mut name_s: String<32> = String::new();
    let _ = name_s.push_str(name);
    Some(Box::new(StockData {
        mode: ChartMode::Quote,
        klines: heapless::Vec::new(),
        last_price: price,
        preclose,
        change: price - preclose,
        change_pct: if preclose > 0.0 { (price - preclose) / preclose * 100.0 } else { 0.0 },
        code: code_s,
        name: name_s,
        quote: Some(Box::new(rq)),
    }))
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
