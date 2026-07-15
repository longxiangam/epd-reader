use alloc::string::String;

use core::fmt::Write;
use embassy_futures::select::{Either, select};

use embassy_net::{
    IpEndpoint, Stack,
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Instant, Timer};
use esp_hal::ram;
use esp_println::println;
use no_std_net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};

use sntpc::{
    NtpContext, NtpTimestampGenerator,
    async_impl::{NtpUdpSocket, get_time},
};
use static_cell::StaticCell;
use time::{Duration, OffsetDateTime, UtcOffset, Weekday};
/*use crate::pages::init_page::InitPage;*/

use crate::sleep::get_sleep_ms;
use crate::weather::{HolidayInfo, Weather};
use crate::wifi::{finish_wifi, use_wifi};

const POOL_NTP_ADDR: [&str; 3] = [
    "ntp.aliyun.com",
    "ntp.tuna.tsinghua.edu.cn",
    "ntp1.aliyun.com",
]; //cn.pool.ntp.org

#[derive(Debug)]
pub enum SntpcError {
    ToSocketAddrs,
    NoAddr,
    UdpSend,
    DnsQuery(embassy_net::dns::Error),
    DnsEmptyResponse,
    Sntc(sntpc::Error),
    BadNtpResponse,
    TimeOut,
}

impl From<SntpcError> for sntpc::Error {
    fn from(err: SntpcError) -> Self {
        match err {
            SntpcError::ToSocketAddrs => Self::AddressResolve,
            SntpcError::NoAddr => Self::AddressResolve,
            SntpcError::UdpSend => Self::Network,
            _ => todo!(),
        }
    }
}

pub(crate) struct Clock {
    sys_start: Mutex<CriticalSectionRawMutex, OffsetDateTime>,
}

impl Clock {
    pub(crate) fn new() -> Self {
        Self {
            sys_start: Mutex::new(OffsetDateTime::UNIX_EPOCH),
        }
    }

    pub(crate) async fn set_time(&self, now: OffsetDateTime) {
        let mut sys_start = self.sys_start.lock().await;
        let elapsed = Instant::now().as_millis();

        *sys_start = now
            .checked_sub(Duration::milliseconds(elapsed as i64))
            .expect("sys_start greater as current_ts");
        // 标记本次开机时钟已恢复（rtc 恢复或 NTP 同步），供界面判断是否可显示，
        // 避免唤醒瞬间 Clock 实例仍是 UNIX_EPOCH 就被读出，显示成 1970/08:00。
        unsafe {
            core::ptr::addr_of_mut!(CLOCK_RESTORED_THIS_BOOT).write(true);
        }
    }

    pub(crate) async fn now(&self) -> OffsetDateTime {
        let sys_start = self.sys_start.lock().await;
        let elapsed = Instant::now().as_millis();
        *sys_start + Duration::milliseconds(elapsed as i64)
    }
    pub async fn local(&self) -> OffsetDateTime {
        self.now()
            .await
            .to_offset(UtcOffset::from_hms(8, 0, 0).unwrap())
    }

    pub(crate) async fn get_week_day(&self) -> String {
        let dt = self.local().await;
        let day_title = match dt.weekday() {
            Weekday::Monday => "周一",
            Weekday::Tuesday => "周二",
            Weekday::Wednesday => "周三",
            Weekday::Thursday => "周四",
            Weekday::Friday => "周五",
            Weekday::Saturday => "周六",
            Weekday::Sunday => "周日",
        };

        let mut result = String::new();

        write!(result, "{day_title}").unwrap();
        result
    }

    pub(crate) async fn get_date_str(&self) -> String {
        let dt = self.local().await;
        let year = dt.year();
        let month = dt.month() as u8;
        let day = dt.day();

        let mut result = String::new();

        write!(result, "{year}-{month}-{day}").unwrap();
        result
    }
}

struct NtpSocket<'a> {
    sock: UdpSocket<'a>,
}

impl<'a> NtpUdpSocket for NtpSocket<'a> {
    fn send_to<T: ToSocketAddrs + Send>(
        &self,
        buf: &[u8],
        addr: T,
    ) -> impl core::future::Future<Output = sntpc::Result<usize>> {
        async move {
            let mut addr_iter = addr
                .to_socket_addrs()
                .map_err(|_| SntpcError::ToSocketAddrs)?;
            let addr = addr_iter.next().ok_or(SntpcError::NoAddr)?;
            self.sock
                .send_to(buf, sock_addr_to_emb_endpoint(addr))
                .await
                .map_err(|_| SntpcError::UdpSend)?;
            Ok(buf.len())
        }
    }

    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl core::future::Future<Output = sntpc::Result<(usize, SocketAddr)>> {
        async move {
            match self.sock.recv_from(buf).await {
                Ok((size, meta)) => {
                    let addr = emb_endpoint_to_sock_addr(meta.endpoint);
                    Ok((size, addr))
                }
                Err(_) => panic!("not exp"),
            }
        }
    }
}

impl<'a> core::fmt::Debug for NtpSocket<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Socket")
            // .field("x", &self.x)
            .finish()
    }
}

fn emb_endpoint_to_sock_addr(endpoint: IpEndpoint) -> SocketAddr {
    let port = endpoint.port;
    let addr = match endpoint.addr {
        embassy_net::IpAddress::Ipv4(ipv4) => {
            let octets = ipv4.octets();
            let ipv4_addr = Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]);
            IpAddr::V4(ipv4_addr)
        }
        _ => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    };

    SocketAddr::new(addr, port)
}

fn sock_addr_to_emb_endpoint(sock_addr: SocketAddr) -> IpEndpoint {
    let port = sock_addr.port();
    let addr = match sock_addr {
        SocketAddr::V4(addr) => {
            let octets = addr.ip().octets();
            embassy_net::IpAddress::v4(octets[0], octets[1], octets[2], octets[3])
        }
        _ => todo!(),
    };
    IpEndpoint::new(addr, port)
}

#[derive(Copy, Clone)]
struct TimestampGen {
    now: OffsetDateTime,
}

impl TimestampGen {
    async fn new(clock: &Clock) -> Self {
        let now = clock.now().await;
        Self { now: now }
    }
}

impl NtpTimestampGenerator for TimestampGen {
    fn init(&mut self) {}

    fn timestamp_sec(&self) -> u64 {
        self.now.microsecond() as u64
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        self.now.microsecond()
    }
}

pub async fn ntp_request(
    stack: &'static Stack<'static>,
    clock: &'static Clock,
) -> Result<(), SntpcError> {
    println!("Prepare NTP request");

    let mut service_index = 0;
    loop {
        if service_index >= POOL_NTP_ADDR.len() {
            return Err(SntpcError::NoAddr);
        }
        let mut addrs = if let Ok(v) = stack
            .dns_query(POOL_NTP_ADDR[service_index], DnsQueryType::A)
            .await
        {
            service_index += 1;
            v
        } else {
            service_index += 1;
            return Err(SntpcError::NoAddr);
        };
        let addr = addrs.pop().ok_or(SntpcError::DnsEmptyResponse)?;
        println!("NTP DNS: {:?}", addr);

        let octets = match addr {
            embassy_net::IpAddress::Ipv4(ip) => ip.octets(),
            _ => [0u8; 4],
        };
        let ipv4_addr = Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]);
        let sock_addr = SocketAddr::new(IpAddr::V4(ipv4_addr), 123);

        let mut rx_buffer = [0; 4096];
        let mut tx_buffer = [0; 4096];
        let mut rx_meta = [PacketMetadata::EMPTY; 16];
        let mut tx_meta = [PacketMetadata::EMPTY; 16];

        let mut socket = UdpSocket::new(
            *stack,
            &mut rx_meta,
            &mut rx_buffer,
            &mut tx_meta,
            &mut tx_buffer,
        );
        socket.bind(1234).unwrap();

        println!("NTP DNS request");

        let ntp_socket = NtpSocket { sock: socket };
        let ntp_context = NtpContext::new(TimestampGen::new(clock).await);

        let get_time_fut = get_time(sock_addr, ntp_socket, ntp_context);
        let timeout_fut = Timer::after_secs(10);

        match select(get_time_fut, timeout_fut).await {
            Either::First(ntp_result) => {
                return match ntp_result {
                    Ok(ntp_result) => {
                        println!("NTP response seconds: {}", ntp_result.seconds);
                        let now =
                            OffsetDateTime::from_unix_timestamp(ntp_result.seconds as i64).unwrap();
                        clock.set_time(now).await;

                        Ok(())
                    }
                    Err(e) => Err(SntpcError::Sntc(e)),
                };
            }
            Either::Second(_) => return Err(SntpcError::TimeOut),
        }
    }
}
#[ram(unstable(rtc_fast))]
static mut WHEN_SLEEP_TIME_TIMESTAMP: u64 = 0;
#[ram(unstable(rtc_fast))]
static mut CLOCK_SYNC_TIME_SECOND: u64 = 0;

static mut CLOCK: Option<&'static Clock> = None;

pub static CLOCK_CELL: StaticCell<Clock> = StaticCell::new();

pub fn get_clock() -> Option<&'static Clock> {
    unsafe { *core::ptr::addr_of!(CLOCK) }
}
pub fn sync_time_success() -> bool {
    unsafe { *core::ptr::addr_of!(CLOCK_SYNC_TIME_SECOND) > 0 }
}

/// 本次开机后 Clock 实例是否已被设置过（rtc 恢复或 NTP 同步）。
/// 与 sync_time_success() 的区别：后者基于 rtc_fast，深睡唤醒后即为 true，
/// 但此时 Clock 实例（普通内存）可能尚未恢复（仍是 UNIX_EPOCH），直接显示
/// 会得到 1970/08:00。本标志在普通内存，每次开机复位为 false，
/// 界面显示时间应基于它判断。
static mut CLOCK_RESTORED_THIS_BOOT: bool = false;
pub fn clock_restored() -> bool {
    unsafe { *core::ptr::addr_of!(CLOCK_RESTORED_THIS_BOOT) }
}

pub async fn save_time_to_rtc() {
    unsafe {
        core::ptr::addr_of_mut!(WHEN_SLEEP_TIME_TIMESTAMP)
            .write(get_clock().unwrap().now().await.unix_timestamp() as u64);
    }
}

#[embassy_executor::task]
pub async fn ntp_worker() {
    let clock = CLOCK_CELL.init(Clock::new());
    unsafe {
        core::ptr::addr_of_mut!(CLOCK).write(Some(clock));
    }
    unsafe {
        let ts = *core::ptr::addr_of!(WHEN_SLEEP_TIME_TIMESTAMP);
        // Sanity check: valid unix timestamps are between year 2020 and 2100
        if ts > 1577836800 && ts < 4102444800 {
            let current_second = ts + get_sleep_ms().await / 1000;
            // 再次校验恢复后的时间落在合理区间：get_sleep_ms 若因 RTC 计时不连续而异常，
            // current_second 会越界，此时跳过恢复，等待 NTP 重新对时（避免设置成错误时间）。
            if current_second > 1577836800 && current_second < 4102444800 {
                if let Ok(now) = OffsetDateTime::from_unix_timestamp(current_second as i64) {
                    clock.set_time(now).await;
                    Timer::after_secs(5).await;
                }
            }
        }
    }
    let mut err_times = 0;
    loop {
        let mut sleep_sec = 3600;
        let sync_time_second = unsafe { *core::ptr::addr_of!(CLOCK_SYNC_TIME_SECOND) };
        if get_clock().unwrap().now().await.unix_timestamp() as u64 - sync_time_second > 3600
            || sync_time_second == 0
        {
            match use_wifi().await {
                Ok(stack) => {
                    println!("NTP Request");
                    match ntp_request(stack, get_clock().unwrap()).await {
                        Err(e) => {
                            finish_wifi().await;
                            println!("NTP error response:{:?}", e);
                            if err_times > 5 {
                                err_times = 0;
                                sleep_sec = 10;
                            } else {
                                sleep_sec = 1;
                            }
                            err_times += 1;
                        }
                        Ok(_) => {
                            finish_wifi().await;
                            println!("NTP ok ?");
                            unsafe {
                                core::ptr::addr_of_mut!(CLOCK_SYNC_TIME_SECOND).write(
                                    get_clock().unwrap().now().await.unix_timestamp() as u64
                                );
                            }
                            err_times = 0;
                            sleep_sec = 3600;
                        }
                    }
                }
                Err(e) => {
                    finish_wifi().await;
                    println!("get stack err:{:?}", e);
                    // 断网/拿不到 stack 时退避重试，避免每秒空转耗电（复用 err_times）。
                    // 即使一直失败，active_time 也不会被复位，设备仍会在 180s 空闲后正常睡眠。
                    if err_times > 5 {
                        err_times = 0;
                        sleep_sec = 3600;
                    } else {
                        err_times += 1;
                        sleep_sec = 10;
                    }
                }
            };
        } else {
            sleep_sec = 3600;
        }

        if sync_time_success() {
            Weather::sync_weather().await;
            HolidayInfo::sync_holiday().await;
        }
        embassy_time::Timer::after(embassy_time::Duration::from_secs(sleep_sec)).await;
    }
}
