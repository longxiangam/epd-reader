use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{ Duration, Instant};
use esp_hal::gpio::{Output, RtcPinWithResistors};
use esp_hal::ram;
use esp_println::println;
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeSource, WakeupLevel};
use heapless::Vec;

use crate::wifi::force_stop_wifi;
use crate::worldtime::save_time_to_rtc;

pub static RTC_MANGE:Mutex<CriticalSectionRawMutex,Option<esp_hal::rtc_cntl::Rtc<'static>>> = Mutex::new(None);
pub static LAST_ACTIVE_TIME:Mutex<CriticalSectionRawMutex,Instant> = Mutex::new(Instant::MAX);
static mut WAKEUP_PINS: Vec<(&'static mut dyn RtcPinWithResistors, WakeupLevel), 5> = Vec::new();

pub static EINK_PWER_PIN: Mutex<CriticalSectionRawMutex, Option<Output<'static>>> = Mutex::new(None);
pub static SD_PWER_PIN: Mutex<CriticalSectionRawMutex, Option<Output<'static>>> = Mutex::new(None);

/// 本次唤醒是否为定时唤醒（PCF 倒计时 INT，或 PCF 布防失败时的 ESP RTC 定时回退）。
/// main 启动时分类并设置；页面据此决定"刷完显示立刻回睡"。
static mut TIMED_WAKE: bool = false;
/// 定时唤醒的数据刷新窗口截止时刻（ms）：窗口内 to_sleep_tips 不放行睡眠
/// （仅天气数据过期需要联网刷新时启用；数据新鲜则无窗口，读完温度立刻睡）
static mut TIMED_HOLD_OFF_UNTIL: u64 = 0;
/// 定时唤醒后用户是否按过键：置位后退出"刷完即睡"，回退普通空闲流程。
/// 开机（set_timed_wake）清零，event::run 每次实体按键置位。
static mut USER_TOUCHED: bool = false;

/// 实体按键事件（event::run 每次按键）调用。
pub fn mark_user_touch() {
    unsafe { USER_TOUCHED = true; }
}

pub fn set_timed_wake(timed: bool, hold_off_ms: u64) {
    unsafe {
        TIMED_WAKE = timed;
        USER_TOUCHED = false; // 每次开机重置：本次会话还没按过键
        TIMED_HOLD_OFF_UNTIL = if timed && hold_off_ms > 0 {
            Instant::now().as_millis() as u64 + hold_off_ms
        } else {
            0
        };
    }
}

/// 定时唤醒的数据刷新窗口（开机后 15 秒）：窗口内 to_sleep_tips 不放行睡眠
/// ——留给 WiFi 连接/天气时间同步/AHT20 首次采样完成。
pub fn timed_hold_off() -> bool {
    unsafe { TIMED_WAKE && (Instant::now().as_millis() as u64) < TIMED_HOLD_OFF_UNTIL }
}

/// 页面空闲超时：定时唤醒的窗口期内 → 永不满足（由 hold-off 决定）；否则用页面配置值
pub fn idle_for(default: Duration) -> Duration {
    if timed_hold_off() {
        Duration::from_secs(3600)
    } else {
        default
    }
}

#[ram(unstable(rtc_fast))]
static mut WHEN_SLEEP_RTC_MS:u64 = 0;

pub async fn refresh_active_time(){
     *LAST_ACTIVE_TIME.lock().await = Instant::now();
}

pub async fn to_sleep(sleep_time:Duration, idle_time:Duration) {
    to_sleep_tips(sleep_time, idle_time, false).await;
}

pub async fn to_sleep_tips(sleep_time:Duration, idle_time:Duration, show_sleep:bool) {
    // 定时唤醒的数据刷新窗口内不放行睡眠（等 WiFi/天气/AHT20 就绪）
    if timed_hold_off() {
        return;
    }
    // 定时唤醒且用户没碰过按键：带 sleep_time>0（定时唤醒时长）的调用来自
    // 刷新+渲染已完成的页面 → 立刻入睡，不再等空闲时间（hold-off 窗口已保证
    // 刷新机会）。main 循环/网格页传 sleep_time=0，永不走此捷径，防止误杀
    // 同步中的页面；用户碰过键或按钮唤醒则回退普通空闲流程。
    let immediate = unsafe { TIMED_WAKE && !USER_TOUCHED && sleep_time.as_secs() > 0 };
    if !immediate {
        if *LAST_ACTIVE_TIME.lock().await == Instant::MAX {
            return;
        }
        if Instant::now().duration_since(*LAST_ACTIVE_TIME.lock().await) <= idle_time {
            return;
        }
    }
    if immediate
        || Instant::now().duration_since(*LAST_ACTIVE_TIME.lock().await) > idle_time
    {
        force_stop_wifi().await;

        let wakeup_pins = unsafe { core::ptr::addr_of_mut!(WAKEUP_PINS).as_mut().unwrap() };
        let rtcio = RtcioWakeupSource::new(wakeup_pins.as_mut_slice());

        // PCF 布防失败时的兜底定时器（先声明：drop 晚于 ws，借用合法）
        let mut fallback_timer: Option<TimerWakeupSource> = None;

        let mut ws: Vec<& dyn WakeSource, 2> = Vec::new();
        ws.push(&rtcio);

        // 定时唤醒改用 PCF8563 倒计时（INT → GPIO4 拉醒 CPU）：PCF 走纽扣电池，
        // 深睡电流更低。布防失败自动退回 ESP RTC 定时唤醒。sleep_time=0 → 不布防。
        if sleep_time.as_secs() > 0 {
            let total = sleep_time.as_secs();
            // ≤255s 走 1Hz 秒档；≤255min 走 1/60Hz 分档（向上取整，最多多睡 59s）；
            // 更长（股票跨天 12h）超出倒计时量程，退回 ESP RTC 定时唤醒。
            let armed = if total <= 255 {
                println!("[sleep] pcf arm {}s (1Hz)", total);
                crate::pcf8563::arm_timer_secs(total as u8).await
            } else if total <= 255 * 60 {
                let mins = ((total + 59) / 60) as u8;
                println!("[sleep] pcf arm {}min (1/60Hz, ={}s)", mins, total);
                crate::pcf8563::arm_timer_minutes(mins).await
            } else {
                println!("[sleep] {}s 超 pcf 量程 → esp rtc 定时", total);
                false
            };
            if !armed {
                println!("[sleep] pcf 布防失败 → esp rtc {}s", total);
                fallback_timer = Some(TimerWakeupSource::new(core::time::Duration::from_secs(
                    total,
                )));
            }
        }
        if let Some(t) = fallback_timer.as_ref() {
            let _ = ws.push(t);
        }
        if show_sleep {
            crate::display::show_sleep().await;
        }

        // 等待渲染周期结束并排空，再切 EINK 电源：异步刷新期间本任务可能并发推进，
        // 若在面板刷新/睡眠命令中途断电，会产生发灰残影（需全刷才能清除）。
        crate::display::await_render_idle().await;

        unsafe {
            *core::ptr::addr_of_mut!(WHEN_SLEEP_RTC_MS) = get_rtc_ms().await;
        }

        // 电源轨断电：板级差异在 PWER 单例的填充侧——v22 只填 SD 槽（轨），
        // 旧板填 EINK+SD 两路；空槽 take 出 None 自动跳过。
        // 带轨入睡会在唤醒瞬间拉塌电池轨 → POR 丢 rtc_fast。
        if let Some(mut p) = EINK_PWER_PIN.lock().await.take() {
            p.set_high();
        }
        if let Some(mut p) = SD_PWER_PIN.lock().await.take() {
            p.set_high();
        }

        save_time_to_rtc().await;

        RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());
    }
}
pub async fn get_rtc_ms()->u64{
    RTC_MANGE.lock().await.as_mut().unwrap().current_time_us() / 1000
}

/// "重启"切换模式：短深睡（50ms 定时器唤醒）。
/// 深睡唤醒会重新跑 main（重新决定起不起 WiFi、重新分配堆），且**保留 rtc_fast**
/// （模式标志/书索引/续读偏移），所以模式切换不需要 software_reset（那会清 rtc_fast）、
/// 也不需要 flash。EINK/SD 电源按正常深睡一样关掉，唤醒后 main 重新上电。
pub async fn reboot_sleep() {
    let wakeup_pins = unsafe { core::ptr::addr_of_mut!(WAKEUP_PINS).as_mut().unwrap() };
    let rtcio = RtcioWakeupSource::new(wakeup_pins.as_mut_slice());
    let timer = TimerWakeupSource::new(core::time::Duration::from_millis(50));
    let mut ws: Vec<&dyn WakeSource, 2> = Vec::new();
    let _ = ws.push(&rtcio);
    let _ = ws.push(&timer);
    // 同 to_sleep_tips：切电前确保面板不在刷新/睡眠中途。
    crate::display::await_render_idle().await;

    // 电源轨断电（同 to_sleep_tips）
    if let Some(mut p) = EINK_PWER_PIN.lock().await.take() {
        p.set_high();
    }
    if let Some(mut p) = SD_PWER_PIN.lock().await.take() {
        p.set_high();
    }

    RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());
}
pub async fn get_sleep_ms()->u64{
    get_rtc_ms().await - unsafe { *core::ptr::addr_of!(WHEN_SLEEP_RTC_MS) }
}

pub async fn add_rtcio(rtcpin: &'static mut dyn RtcPinWithResistors, wakeup_level: WakeupLevel){
    unsafe {
        (*core::ptr::addr_of_mut!(WAKEUP_PINS)).push((rtcpin, wakeup_level));
    }
}
