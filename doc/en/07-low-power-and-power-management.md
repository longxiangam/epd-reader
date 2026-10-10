# Part 7: Low-Power Design and Power Management

> 中文版: [../07-低功耗设计与电源管理.md](../07-低功耗设计与电源管理.md)

> Building an ESP32-C3 E-Ink Reader in Rust · Article Series

## Introduction

Battery power is one of the most basic requirements for embedded devices. This project runs on a single lithium battery; the ESP32-C3 draws roughly 80-100mA with WiFi on, but only 5-10μA in deep sleep — four orders of magnitude apart.

How do we maximize the time between charges? There are two answers: exploit the e-ink display's "keeps its image without power" property, and design the sleep strategy carefully. This article covers deep sleep, RTC memory, peripheral power control, battery management, and other key techniques.

## 1. Deep Sleep

### 1.1 Sleep Trigger Conditions

All pages go to sleep through `to_sleep_tips`, which takes two key parameters:

- `sleep_time`: the timed-wake interval. Greater than 0 means "after rendering this frame, sleep this long and wake up to refresh again" (arms a PCF timed wake); equal to 0 means only the idle timeout or a key press can wake the device.
- `idle_time`: how long the device must be idle before it counts as "nobody is using it"; sleep is only allowed past this threshold.

The main loop checks idle time every 5 seconds. If no input has occurred for over 180 seconds (3 minutes), it goes to sleep:

```rust
// src/main.rs:392-398 (main loop)
to_sleep_tips(
    Duration::from_secs(0),
    crate::sleep::idle_for(Duration::from_secs(180)),
    true,
).await;
Timer::after(Duration::from_secs(5)).await;
```

`idle_for` is a wrapper around the idle threshold: during the data-refresh window after a timed wake it returns 3600 seconds — the idle condition is never satisfied inside the window, and the "sleep immediately" shortcut takes over (see 1.5).

Page-level sleep times differ:

```rust
// src/pages/main_page.rs:282 (main menu)
to_sleep_tips(Duration::from_secs(0), crate::sleep::idle_for(Duration::from_secs(30)), true).await;

// src/pages/read/page.rs:2554-2568 (while reading; idle threshold configurable, default 120 s)
to_sleep_tips(Duration::from_secs(0), Duration::from_secs(read_sleep_seconds), true).await;

// src/pages/stock/page.rs:614-619 (stock page; sleep_time>0 → timed wake)
to_sleep_tips(
    Duration::from_secs(sleep_secs),
    crate::sleep::idle_for(Duration::from_secs(idle_secs)),
    true,
).await;
```

- The main menu sleeps after 30 seconds of inactivity; the reading page is configurable (default 120 seconds, minimum 10 seconds, 5-second steps).
- The weather/calendar timed-wake periods are covered in 5.3; the stock page decides by trading session (120 seconds while the market is open, otherwise sleep until the next open).
- Choosing "Sleep" in the reading menu passes `idle_time=0` and sleeps immediately.

### 1.2 Sleep Flow

`to_sleep_tips` is the entry point for sleep:

```rust
// src/sleep.rs:73-171 (simplified)
pub async fn to_sleep_tips(sleep_time: Duration, idle_time: Duration, show_sleep: bool) {
    // Inside the timed-wake data-refresh window, sleep is not allowed (wait for WiFi/weather/AHT20)
    if timed_hold_off() { return; }

    // Timed wake and the user has not touched anything: the page has finished
    // refreshing + rendering → sleep right away (see 1.5)
    let immediate = unsafe { TIMED_WAKE && !USER_TOUCHED && sleep_time.as_secs() > 0 };
    if !immediate {
        if *LAST_ACTIVE_TIME.lock().await == Instant::MAX { return; }
        if Instant::now().duration_since(*LAST_ACTIVE_TIME.lock().await) <= idle_time { return; }
    }

    if immediate || Instant::now().duration_since(*LAST_ACTIVE_TIME.lock().await) > idle_time {
        // 1. Stop WiFi
        force_stop_wifi().await;

        // 2. Configure wake sources: GPIO keys + PCF/RTC timer
        let wakeup_pins = unsafe { core::ptr::addr_of_mut!(WAKEUP_PINS).as_mut().unwrap() };
        let rtcio = RtcioWakeupSource::new(wakeup_pins.as_mut_slice());
        let mut ws: Vec<&dyn WakeSource, 2> = Vec::new();
        ws.push(&rtcio);
        // ... PCF arming tiers (see 1.3); on failure, fall back to the ESP RTC timer

        // 3. Render the standby screen (the e-ink highlight)
        if show_sleep { crate::display::show_sleep().await; }

        // 4. Wait for the render cycle to drain: cutting power mid-refresh leaves gray ghosts
        crate::display::await_render_idle().await;

        // 5. Save the current RTC timestamp
        unsafe { *core::ptr::addr_of_mut!(WHEN_SLEEP_RTC_MS) = get_rtc_ms().await; }

        // 6. Cut the power rails (board differences in section 3; empty slots skip automatically)
        if let Some(mut p) = EINK_PWER_PIN.lock().await.take() { p.set_high(); }
        if let Some(mut p) = SD_PWER_PIN.lock().await.take() { p.set_high(); }

        // 7. Save the time into RTC registers
        save_time_to_rtc().await;

        // 8. Enter deep sleep
        RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());
    }
}
```

Flow diagram:

```
Idle timeout detected (or the timed wake "sleep immediately" shortcut)
    │
force_stop_wifi() ←── stop WiFi (the biggest power saver)
    │
Configure wake sources ←── GPIO keys + PCF timer/alarm (ESP RTC timer as fallback)
    │
show_sleep() ←── render the standby screen on the e-ink panel
    │
await_render_idle() ←── let the panel finish its refresh/sleep commands, prevent ghosts
    │
Save RTC time ←── record the timestamp of entering sleep
    │
Cut the power rails ←── screen and SD card powered down together
    │
save_time_to_rtc() ←── write system time into the RTC registers
    │
sleep_deep() ←── CPU stops; only the RTC domain stays powered
```

The order of steps 3 and 6 matters: render the standby screen first, then cut the display's power. Because an e-ink panel keeps its image without power, that image remains until the next wake — that is exactly how the "standby screen" works. The standby image itself is customizable: pages that registered a `sleep_renderer` (weather/calendar/stock/reading) render a low-density layout for the standby frame; pages without one fall back to the sleep bitmap stored in a flash partition (`flash_sleep::draw_sleep_image`), and if even that fails, to a plain "Sleeping" text.

### 1.3 Sleep Arming Tiers (PCF8563 Timed Wake)

When `sleep_time > 0`, a timed wake must be armed. Timed wakes are handled by the PCF8563 hardware RTC (its INT pin pulls GPIO4 low to wake the CPU): the PCF runs on a coin cell (CR2032), so it draws less current than the MCU's deep-sleep clock, and its 32k crystal keeps accurate time — the MCU's deep-sleep slow clock drifts too much for long intervals. Three tiers by sleep duration:

| Sleep duration | Arming method | Notes |
|--------|---------|------|
| ≤255 seconds | PCF countdown, 1 Hz (`arm_timer_secs`) | register 0x0F loads the second count; second-accurate |
| ≤255 minutes | PCF countdown, 1/60 Hz (`arm_timer_minutes`) | minutes rounded up, at most 59 s extra sleep |
| Longer (with PCF) | PCF absolute-time alarm (`arm_alarm_at`) | scheduled against the PCF's own clock (UTC) |
| No PCF (v1) / arming failure | ESP RTC `TimerWakeupSource` | automatic fallback |

```rust
// src/sleep.rs:107-141 (simplified)
let armed = if total <= 255 {
    crate::pcf8563::arm_timer_secs(total as u8).await
} else if total <= 255 * 60 {
    let mins = ((total + 59) / 60) as u8;      // round up
    crate::pcf8563::arm_timer_minutes(mins).await
} else if crate::board::HAS_PCF {
    match crate::pcf8563::read_time().await {
        Some(pcf_now) => crate::pcf8563::arm_alarm_at(pcf_now + total as i64).await,
        None => false,                          // PCF time invalid → ESP RTC fallback
    }
} else {
    false                                       // v1 has no PCF → ESP RTC fallback
};
if !armed {
    fallback_timer = Some(TimerWakeupSource::new(core::time::Duration::from_secs(total)));
}
```

The absolute-time alarm writes the alarm registers 0x09-0x0C (minute/hour/day BCD compare, the weekday register written as 0x80 to disable compare) and enables AIE. Note that the PCF8563 alarm has **no month register**: for sleep intervals longer than 28 days, the device wakes once early on the same day of the following month — it simply re-arms on waking, which is harmless.

With `sleep_time = 0` no timer is armed and only a key press can wake the device.

### 1.4 Wake Sources

The ESP32-C3 supports two kinds of wake sources, and both boards register them:

**GPIO wake** (key press):

```rust
// src/main.rs:306-309
// Register the RTC wake key before the reading branch — reading-mode sleep
// (to_sleep_tips/reboot_sleep) also relies on this RTC IO wake source,
// otherwise the device could never wake up once asleep.
let rtc_io = static_cell::make_static!(crate::board::key_ladder_pin());
add_rtcio(rtc_io, WakeupLevel::Low).await;  // wake on low level (key pressed)
```

The wake pin is the three-key resistor ladder (which key via ADC divider levels): GPIO4 on the v2 board, GPIO2 on the v1 board. Pressing a key pulls it low and triggers the RTC IO wake. On the v2 board the PCF8563's INT pin is wired to the same GPIO4 — timed wake and key wake share one wake path, and are told apart by the classification logic in 1.6.

**Timer wake**:

PCF countdown/alarm expiry → INT pulls GPIO4 low (section 1.3); when the PCF is absent or arming fails, the ESP RTC timer takes over. With `sleep_time` = 0 nothing is armed and only a key press wakes the device.

### 1.5 Sleeping Immediately After a Timed Wake

The point of a timed wake is "wake → refresh over the network → re-render → sleep again". If the device still had to wait out the full 3-minute idle timeout after finishing, it would stay lit for minutes for nothing. So `sleep.rs` has a shortcut:

```rust
// src/sleep.rs:22-60
static mut TIMED_WAKE: bool = false;      // was this wake a timed wake?
static mut TIMED_HOLD_OFF_UNTIL: u64 = 0; // data-refresh window deadline (ms)
static mut USER_TOUCHED: bool = false;    // has the user pressed a key since the timed wake?

/// Called by physical key events (event::run, on every key press).
pub fn mark_user_touch() { unsafe { USER_TOUCHED = true; } }

/// The timed-wake data-refresh window: while it lasts, to_sleep_tips refuses to sleep
/// — it leaves time for the WiFi connection / weather-time sync / first AHT20 sample.
pub fn timed_hold_off() -> bool {
    unsafe { TIMED_WAKE && (Instant::now().as_millis() as u64) < TIMED_HOLD_OFF_UNTIL }
}

/// Page idle timeout: during a timed wake's window → never satisfied; otherwise the page's value
pub fn idle_for(default: Duration) -> Duration {
    if timed_hold_off() { Duration::from_secs(3600) } else { default }
}
```

The sleep decision:

```rust
// src/sleep.rs:82
let immediate = unsafe { TIMED_WAKE && !USER_TOUCHED && sleep_time.as_secs() > 0 };
```

All three conditions must hold to take the shortcut:

1. This wake was a timed wake (`TIMED_WAKE`);
2. The user has not touched any key (`!USER_TOUCHED`; the key task in `event.rs` calls `mark_user_touch()` on every physical key press);
3. The caller passed `sleep_time > 0` — meaning a page that "finished its refresh" is requesting the timed return to sleep.

The main loop and main menu always pass `sleep_time = 0`, so they never take the shortcut — this prevents killing pages that are mid-sync. When the shortcut applies, the idle check is skipped and the device sleeps as soon as refresh + rendering complete; if the user touched a key, it falls back to the normal idle flow (if you want to look, it stays on a while longer).

The refresh window (hold-off) is only enabled when the weather data is stale: at startup, main gives an 8-second window for the WiFi connection plus weather/time sync when `Weather::weather_stale()` is true, and no window at all when the data is fresh — the device can go back to sleep right after reading the temperature.

### 1.6 Wake Classification and First-Frame Handling

After waking, the program restarts from the top of `main()` (it does not resume from the sleep point). Early in startup it classifies the wake:

```rust
// src/main.rs:179-196
// Deep-sleep wake classification: PCF8563 TF=1 → PCF timed wake (PCF INT pulled GPIO4 low);
// ESP RTC Timer → the ESP timer fallback used when PCF arming failed; otherwise a button wake.
// Read the flag, clear it, stop the timer (INT released, preventing phantom keys at runtime)
let timed_wake = crate::pcf8563::consume_timer_wake().await
    || matches!(wake_reason, esp_hal::system::SleepSource::Timer);
// Only stale weather data requires a network refresh (8 s window for WiFi + sync);
// with fresh data there is no window: sleep right after re-rendering the temperature
let stale = crate::weather::Weather::weather_stale().await;
crate::sleep::set_timed_wake(timed_wake, if stale { 8_000 } else { 0 });
// Non-timed wake (button/power-on) entering the app: force a full refresh on the first
// frame to clear ghosts from the previous session; timed wakes keep fast updates
// (a full refresh on every minute-level wake would flicker and waste power).
if !timed_wake {
    crate::display::mark_need_clear();
}
```

`consume_timer_wake` reads the PCF8563 Control_status_2 register to check the TF (countdown expired) or AF (alarm fired) flags, then clears TF/AF, stops the timer, and disables AIE/TIE — if the timer were left running, its INT pulling GPIO4 low at runtime would be misread by the key network as a phantom key press.

The classification drives two behaviors:

- **Timed wake**: sets `TIMED_WAKE` and the refresh window (1.5); the first frame stays a fast update (a full refresh on every minute-level wake would flicker and waste power).
- **Non-timed wake** (button/power-on): `mark_need_clear()` forces a full refresh on the first frame, wiping ghosts left on the panel by the previous session.

### 1.7 Restart-Based Mode Switching: reboot_sleep

"Switching run modes" (entering/exiting reading) is also done with deep sleep, but for only 50 milliseconds:

```rust
// src/sleep.rs:180-199 (simplified)
pub async fn reboot_sleep() {
    let rtcio = RtcioWakeupSource::new(wakeup_pins.as_mut_slice());
    let timer = TimerWakeupSource::new(core::time::Duration::from_millis(50));
    let mut ws: Vec<&dyn WakeSource, 2> = Vec::new();
    ws.push(&rtcio);
    ws.push(&timer);
    crate::display::await_render_idle().await;   // same as to_sleep_tips: panel idle before power cut
    // Cut the power rails (same as to_sleep_tips)
    ...
    RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());
}
```

A deep-sleep wake reruns `main` (re-deciding whether to bring up WiFi, re-allocating the heap) and **preserves rtc_fast** (mode flags/book index/resume offset), so mode switching does not need `software_reset` — that would wipe rtc_fast. When the user picks "e-book" in the main menu, the code sets the reading flag + writes the NVS sentinel; when the 50 ms `reboot_sleep` wake reruns `main`, it detects the flag and enters reading mode directly (no WiFi init, the whole heap goes to TTF rendering). Exiting reading switches back the same way.

## 2. RTC Fast Memory

RTC fast memory is a special SRAM block (about 8KB) on the ESP32-C3 that stays powered during deep sleep. Variables placed there keep their values across a wake.

```rust
// src/display.rs:49-50
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES: u32 = 0;
```

RTC variables in this project (excerpt):

| Variable | File | Purpose |
|------|------|------|
| `RENDER_TIMES` | display.rs | fast-refresh counter (decides when to full-refresh) |
| `NEED_CLEAR` | display.rs | cold-boot clear flag (forces a full refresh on the first frame) |
| `LAST_BATTERY_PERCENT` | battery.rs | last battery percentage |
| `WHEN_SLEEP_RTC_MS` | sleep.rs | RTC timestamp taken when entering sleep |
| `PAGE_INDEX` / `BOOTED` | main_page.rs | selected main-menu item / cold-boot flag |
| `READING_MODE` / `PAGE_INDEX` / `TTF_RESUME_OFFSET` | read/page.rs | reading-mode flag / book index / resume byte offset |
| `RESUME_SUBDIR` / `LIST_SUBDIR` / `LIST_BOOK_IDX` | read/page.rs | resume subdirectory / book-list level & book index memory |
| `STOCK_MODE` / `STOCK_VIEW` / `STOCK_MINUTE_CACHE` | stock/page.rs | stock chart mode / view / 6-stock minute cache |
| `WHEN_SLEEP_TIME_TIMESTAMP` etc. | worldtime.rs | time restoration and sync flags |
| `RTC_FAST_BOOT_SEQ` | board/mod.rs | diagnostics: boot sequence counter |

### RTC Memory Usage Pattern

```rust
// Write
unsafe {
    *core::ptr::addr_of_mut!(WHEN_SLEEP_RTC_MS) = get_rtc_ms().await;
}

// Read
pub async fn get_sleep_ms() -> u64 {
    get_rtc_ms().await - unsafe { *core::ptr::addr_of!(WHEN_SLEEP_RTC_MS) }
}
```

After waking, `get_sleep_ms()` computes how long the device slept; adding that to the pre-sleep system time restores the current clock — no NTP sync needed on every wake (on the v2 board, clock restoration prefers the PCF8563 hardware clock, see 5.3).

### Two Ways rtc_fast Gets Lost

- **software_reset**: a soft reset clears rtc_fast. That is exactly why the mode switch in 1.7 uses `reboot_sleep` (deep sleep) instead of a soft reset.
- **RTC IO wake on the v2 board**: in practice, an IO (RTCIO) wake loses rtc_fast state (`BOOTED=false`, the reading flag disappears). Critical state therefore keeps an NVS sentinel copy: the page is written via `write_sleep_page` whenever it changes, and after waking `read_sleep_page` restores the last page; "was reading" is marked by the `PAGE_READING` sentinel, which main checks at startup to enter reading mode directly.

There is also a boot-incrementing diagnostic, `RTC_FAST_BOOT_SEQ`: still incrementing across deep-sleep wakes = preserved; back to 1 = lost during deep sleep.

## 3. Peripheral Power Control (Power Rails)

The ESP32-C3 has no independent power domain per peripheral, so this project drives external MOSFETs via GPIO to cut power to the screen/SD card. The two board generations differ:

| Board | Power rails | Polarity | Action before sleep |
|------|--------|------|---------|
| v1 (old) | dual rail: EINK=GPIO21 / SD=GPIO1 | high = off | both rails set high (off) |
| v2 (new) | single rail: GPIO1 | low = on | set high (off) |

```rust
// src/board/v1.rs:39-49 (v2's init_power_rails fills both slots with the same GPIO1)
pub async fn init_power_rails(p: &mut Peripherals) {
    let mut eink_pwr_ctrl = Output::new(p.GPIO21, Level::High, OutputConfig::default());
    let mut sd_pwr_ctrl = Output::new(p.GPIO1, Level::High, OutputConfig::default());
    eink_pwr_ctrl.set_low();   // low = power on (MOSFET conducting)
    sd_pwr_ctrl.set_low();
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}
```

The sleep side is board-agnostic: `to_sleep_tips` `take`s the `EINK_PWER_PIN`/`SD_PWER_PIN` slots and sets them all high; empty slots are skipped automatically — on v1 these are two physical rails, on v2 one rail fills both slots, with an equivalent effect.

**Why the rails must be cut before deep sleep**: sleeping with a rail held low (GPIO1 driven low) collapses the battery rail at the instant of wake, triggering a POR — and rtc_fast is lost entirely. So `to_sleep_tips`/`reboot_sleep` both cut the rails before `sleep_deep`, and main re-powers them after waking.

**Pad hold, per board**:

```rust
// src/board/v2.rs:24-28: release the hold laid down by the previous deep sleep at boot
// (the RTC domain keeps it across deep sleep — if not released, the low-level drive
// onto GPIO1 cannot be written → rail off → display/SD dead)
pub fn init_pad_hold(_p: &mut Peripherals) {
    esp_hal::peripherals::LPWR::regs().pad_hold().modify(|_, w| unsafe { w.bits(0) });
}

// src/board/v1.rs:25-28: the old board sets a hold on GPIO1 (SD rail) to keep
// the rail state during deep sleep
pub fn init_pad_hold(p: &mut Peripherals) {
    unsafe { p.GPIO1.clone_unchecked() }.rtcio_pad_hold(true);
}
```

**GPIO2 deep-sleep isolation** (v2 board, battery divider pin):

```rust
// src/main.rs:244-258
// GPIO2 is a strapping pin with a default internal pull-up after hardware reset — if,
// after the digital domain loses power in deep sleep, the pad reverts to that reset
// default, it would pull the divider midpoint (VBAT/2≈1.7V) up to ~3V, adding leakage.
// The explicit sleep config lives in the RTC domain and forces high-impedance in deep sleep.
let io_mux = esp_hal::peripherals::IO_MUX::regs();
io_mux.gpio(2).modify(|_, w| {
    w.slp_sel().set_bit();      // use the sleep config in deep sleep
    w.mcu_ie().clear_bit();     // input off
    w.mcu_wpu().clear_bit();    // pull-up off
    w.mcu_wpd().clear_bit()     // pull-down off
});
```

At runtime the pad still uses the analog config, so ADC sampling is unaffected; these bits are set once at boot and last for the session (the `set_analog` path and this isolation never touch each other's bits).

## 4. WiFi Power Strategy

WiFi is the largest power consumer. The project implements an auto-off mechanism:

```
Network needed → use_wifi() (take lock + refresh timestamp)
     │
Done → finish_wifi() (release lock)
     │
do_stop task checks every 3 seconds
     │
No new network request within 30 seconds?
     │
Yes → STOP_WIFI_SIGNAL → disconnect WiFi → WifiStopped
     │
Next time the network is needed → RECONNECT_WIFI_SIGNAL → reconnect
```

This way, WiFi is only on when actually needed. While the settings page's web configuration service is running, it periodically calls `refresh_last_time()` to keep alive, so it is not cut off mid-configuration by the auto-off.

## 5. Network Sync Scheduling

### 5.1 ntp_worker's Scheduling Strategy

`ntp_worker` is the scheduling hub for all network sync. Its loop interval determines how often network requests happen:

```rust
// src/worldtime.rs:417-482 (simplified)
loop {
    let mut sleep_sec = 3600;  // default: 1 hour

    // NTP sync: last sync more than 1 hour ago, or never synced
    if now - sync_time_second > 3600 || sync_time_second == 0 {
        match use_wifi().await {
            Ok(stack) => match ntp_request(stack, clock).await {
                Ok(_) => {
                    // Sync OK → write the time into the PCF8563 (kept by the coin
                    // cell, survives wakes/power loss)
                    crate::pcf8563::write_time(ts).await;
                    sleep_sec = 3600;            // success: sync again in 1 hour
                }
                Err(_) => { sleep_sec = 1; }     // failure: retry in 1 second
            },
            Err(_) => { sleep_sec = 10; }        // no network: back off, avoid busy-spinning
        }
    }

    // After a successful NTP sync, trigger weather and holiday sync
    if sync_time_success() {
        Weather::sync_weather().await;
        HolidayInfo::sync_holiday().await;
    }

    Timer::after(Duration::from_secs(sleep_sec)).await;
}
```

### 5.2 Weather and Holiday Sync

The weather sync interval is controlled by `Weather::sync_weather()`:

```rust
// src/weather.rs:158-166 (simplified)
pub async fn sync_weather() {
    // First boot: load the cached timestamp from flash
    if WEATHER_SYNC_SECOND == 0 { Self::get_weather().await; }

    // Only issue a new request if the last weather sync was more than 5 hours ago
    if !sync_weather_success() || now - WEATHER_SYNC_SECOND > 3600 * 5 {
        let _ = Weather::request().await;
    }
}
```

The actual sync intervals:

| Sync item | Interval | Notes |
|--------|------|------|
| NTP time | 1 hour | `ntp_worker` main loop, 3600 seconds |
| Weather data | 5 hours | `3600 * 5 = 18000` seconds |
| Holidays | 1 year | re-fetches when the year changes |
| NTP retry on failure | 1 second | drops to 10 seconds after 5 consecutive failures |
| Offline (no stack) | 10 seconds | backoff retry; normal sleep still applies after 180 s idle |

WiFi is released via `finish_wifi()` immediately after each sync completes, starting the 30-second countdown to auto-off. At one weather update per 5 hours, WiFi turns on only about 5 times a day, for a few seconds each.

### 5.3 Per-Page Timed-Wake Periods

The timed-wake period (the `sleep_time` of section 1.3) is decided by the foreground page:

| Page | Timed-wake period | Notes |
|------|-------------|------|
| Weather/calendar | time display on: aligned to the next minute boundary (≥ idle threshold) | the minute flips right at wake |
| Weather/calendar | time display off: sleep until the weather data expires (`stale_in_secs`, at least 60 s) | matches the 5-hour sync interval |
| Stock (market open) | 120 seconds | trading days 9:15~11:31 / 12:55~15:01 (includes the call auction, 1-minute endpoint margin) |
| Stock (market closed) | sleep until the next market open | lunch break → 12:55 same day; after close/weekend/holiday → 9:25 on the next trading day (holiday-data aware, scans 30 days, 12 h fallback) |
| Stock (day/week/month K) | 12 hours | non-realtime modes |
| Reading | no timed wake | `sleep_time=0`, key press only |

On the v2 board the PCF8563 is also the system clock source: after a successful NTP sync the UTC time is written into the PCF (kept by the CR2032), and every wake restores from it first — deep-sleep drift, reading mode without WiFi, and battery swaps no longer lose the time. Reading mode does not run `ntp_worker`; `init_clock_pcf` restores the clock from the PCF.

## 6. Battery Management

### 6.1 ADC Voltage Sensing

The battery feeds an ADC pin through a resistor divider (board-defined: v2=GPIO2, v1=GPIO4):

```rust
// src/main.rs:204-208
let bat_adc1_pin = adc1_config
    .enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
        crate::board::bat_pin(),
        Attenuation::_11dB,
    );
```

The ADC uses curve calibration (`AdcCalCurve`) for accuracy; 11dB attenuation allows measuring signals close to the supply voltage.

### 6.2 Battery Percentage

```rust
// src/battery.rs:37-64 (simplified)
const V_MAX: u32 = 4100;   // mV
const V_MIN: u32 = 3100;

let voltage_mv = adc_value as f32 * 2.0;    // divider ratio 1:2
let percent = if voltage_mv > V_MAX as f32 {
    100
} else if voltage_mv < V_MIN as f32 {
    0
} else {
    // Square curve: fits the lithium discharge plateau (voltage changes slowly mid-range)
    let normalized = (voltage_mv - V_MIN as f32) / (V_MAX - V_MIN) as f32;
    (normalized * normalized * 100.0).round() as u32
};
v.percent = percent.clamp(0, 100);
unsafe { *core::ptr::addr_of_mut!(LAST_BATTERY_PERCENT) = v.percent; }
```

The battery percentage is stored in RTC memory, and the render path can read it synchronously via `battery::last_percent()` (no await needed). Below 20% a low-battery warning is printed. Because e-ink refreshes are slow, battery info does not need real-time updates.

### 6.3 Periodic Sampling

```rust
// src/battery.rs:94 (tail of the sampling task loop)
Timer::after_secs(60).await;  // sample every 60 seconds
```

A 60-second sampling period balances freshness and power. Occasional ADC read errors are retried (up to 50 times); persistent failure keeps the last percentage.

## 7. Power Analysis

Estimated power draw per state:

| State | Power | Duration |
|------|------|---------|
| WiFi on + rendering | ~100mA | a few seconds at a time |
| Rendering (no WiFi) | ~30mA | a few hundred ms at a time |
| Idle (CPU running, panel off) | ~10mA | brief |
| Deep sleep (PCF timing) | ~10μA | the vast majority of the time |

With a 1000mAh lithium battery:
- Weather refresh every 5 hours (WiFi on about 5 s): 100mA × 5s / 18000s ≈ 0.028mA average
- Minute-level timed wakes (time display on): a few seconds of rendering + network refresh per wake; the average current still stays well below 0.1mA
- User interaction (keys, page turns) at 30 minutes/day: 30mA × 0.5h / 24h ≈ 0.625mA average
- Deep sleep 99%+ of the time: 10μA ≈ negligible
- **Estimated runtime: weeks to months** (depending on usage)

Several designs keep "awake time" as short as possible: sleeping immediately after a timed wake (1.5) removes the post-refresh idle wait; the stock page sleeps straight through to the next market open instead of spinning; timed wakes keep a fast first frame, saving both time and power. For occasional e-book reading (infrequent networking), the runtime can reach several weeks or more.

## Summary

| Technique | Key point |
|--------|------|
| Deep sleep | `sleep_deep()` + RTC domain kept powered |
| Sleep arming | PCF8563 tiers: seconds/minutes/absolute alarm, ESP RTC timer fallback |
| Wake sources | GPIO RTC wake (key ladder) + PCF INT (→GPIO4) + timer fallback |
| Immediate re-sleep | timed wakes sleep right after refreshing; a key touch reverts to the idle flow |
| Wake classification | `consume_timer_wake` reads TF/AF; non-timed wakes force a first-frame full refresh |
| Mode switching | `reboot_sleep`, a 50 ms short deep sleep that keeps rtc_fast — no software_reset |
| RTC memory | `#[ram(unstable(rtc_fast))]`, preserved in deep sleep; lost on IO wake → NVS sentinel |
| Power rails | v1 dual rail / v2 single rail, cut before sleep (prevents collapsing the battery rail at wake) |
| Deep-sleep pins | GPIO2 isolation (slp_sel + IE/WPU/WPD off); pad hold handled per board |
| WiFi power | auto-off after 30 s without requests; reconnect when needed |
| Battery | ADC sampling + curve calibration, 60 s period, square curve fits the discharge plateau |
| E-ink | keeps its image unpowered; the standby screen is rendered before sleeping |

The core idea of low-power design: **keep the CPU and peripherals in deep sleep as much as possible, and wake only briefly when needed**. The e-ink display's "keeps its image without power" property pairs perfectly with deep sleep — the frame rendered before sleeping is the device's "face".

In the next article, we will discuss the UI component system and page system.

---

> Previous: [Part 6: Data Persistence and File Systems](../06-数据持久化与文件系统.md) · Next: [Part 8: UI Components and the Page System](08-ui-components-and-page-system.md)
