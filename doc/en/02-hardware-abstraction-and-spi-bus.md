# Part 2: Hardware Abstraction and SPI Bus Sharing

> 中文版: [../02-硬件抽象与SPI总线共享.md](../02-硬件抽象与SPI总线共享.md)
>
> Building an ESP32-C3 E-Ink Reader in Rust · Article Series

## Introduction

Touching hardware is where embedded development most easily goes wrong: two modules driving the same SPI bus at once, conflicting register configuration, a GPIO pulled high and low by two sides simultaneously — these problems usually only surface at runtime and are hard to debug.

esp-hal uses Rust's type system and ownership to turn some of these into compile-time errors. This article covers the hardware abstraction in this project: first the board abstraction that composes two board generations out of features, then how one SPI bus is shared safely between the e-ink display and the SD card, how one I2C bus hosts the PCF8563 clock and the AHT20 sensor, and finally the patterns used for global state management in embedded Rust.

## 1. esp-hal v1.x Hardware Abstraction Design

### 1.1 Taking peripherals: the first step of ownership

On a PC you might poke hardware through system calls. On bare-metal embedded, hardware peripherals are registers mapped directly into memory. esp-hal wraps these low-level details behind a type-safe interface.

Everything starts with `hal_init`:

```rust
// src/main.rs:109-110
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let mut peripherals = hal_init(config);
```

`hal_init` returns a `Peripherals` struct containing every ESP32-C3 hardware peripheral. The key point: **each peripheral can be taken exactly once**. Internally `hal_init` uses Rust's `take()` pattern — a second call panics.

This means there is exactly one `Peripherals` instance in the whole program, and its fields use move semantics so each peripheral has exactly one owner. Note that `peripherals` is a mutable binding: the board-init functions that follow need `&mut Peripherals` to configure pins (see Section 2).

### 1.2 GPIO: from Peripherals to usable pins

In esp-hal v1.x, GPIO pins come straight from `Peripherals`:

```rust
// src/main.rs:152-160
let epd_busy = peripherals.GPIO6;          // input pin, just take it
let epd_rst = peripherals.GPIO7;           // wrapped into an Output later
let epd_dc = peripherals.GPIO20;
let epd_cs = Output::new(                  // output pin, configured immediately
    peripherals.GPIO3,
    Level::High,                           // initial level: high (deselected)
    OutputConfig::default(),               // default output config
);
let sdcard_cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
```

Ownership transfer in action: once `peripherals.GPIO6` is "moved" into `epd_busy`, `peripherals` no longer holds GPIO6. Any later attempt to access `peripherals.GPIO6` fails to compile.

`Output::new` is the new esp-hal v1.x API with three parameters:
- **pin**: the GPIO pin (ownership moves in)
- **initial_level**: the initial level
- **OutputConfig**: output configuration (open drain, pull, etc.)

`OutputConfig` is new in v1.x; older versions needed only two arguments. This builder-style config struct is ubiquitous across esp-hal v1.x.

### 1.3 Input: configured input pins

```rust
// src/event.rs:129 (inside the v2 key-scan task)
let mut key_all = Input::new(key2, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
```

Button pins are configured as pulled-up inputs — high when released, low when pressed. `InputConfig::default().with_pull(Pull::Up)` is the esp-hal v1.x style, more extensible than passing `Pull::Up` directly as older versions did.

One detail: the key-ladder pin doubles as an ADC input (to tell keys apart). ADC's `set_analog` strips the digital configuration, so `main` re-applies the digital pull-up input to the same pin after ADC init:

```rust
// src/main.rs:216-220
let k2_in = Input::new(
    crate::board::key_ladder_pin(),
    InputConfig::default().with_pull(Pull::Up),
);
core::mem::forget(k2_in);   // only the configuration must stick; the handle is dropped
```

### 1.4 SPI: bus configuration

SPI is this project's most critical bus — both the e-ink display and the SD card hang off it:

```rust
// src/main.rs:163-172
let spi = Spi::new(
    peripherals.SPI2,
    SpiConfig::default()
        .with_frequency(Rate::from_mhz(32))   // 32MHz clock
        .with_mode(Mode::_0),                  // SPI Mode 0 (CPOL=0, CPHA=0)
).unwrap()
.with_sck(epd_sclk)    // GPIO8  clock
.with_miso(epd_miso)   // GPIO10 data in
.with_mosi(epd_mosi);  // GPIO0  data out
```

esp-hal v1.x configures SPI with the `SpiConfig` builder pattern:
- `with_frequency(Rate::from_mhz(32))`: 32MHz is a high rate SPI2 supports
- `with_mode(Mode::_0)`: note the PascalCase variant `_0` — Edition 2024 convention
- `.with_sck/miso/mosi()`: chained calls assign pins

**Note**: once the SPI instance is created, ownership of `peripherals.SPI2` has moved; it cannot be created again. The pins `epd_sclk`, `epd_miso`, and `epd_mosi` are consumed too.

Another initialization-order detail: MISO (GPIO10) needs its internal pull-up enabled before SPI claims it — before initialization the SD card's DO is high-impedance, and without a pull-up the CMD0 response read is unreliable:

```rust
// src/board/mod.rs
pub fn sd_miso_pullup(p: &mut esp_hal::peripherals::Peripherals) {
    let miso_pu = Input::new(
        unsafe { p.GPIO10.clone_unchecked() },
        InputConfig::default().with_pull(Pull::Up),
    );
    core::mem::forget(miso_pu); // configuration stays for the whole runtime
}
```

## 2. Board Abstraction: board_v1 / board_v2

The project has two hardware generations; `src/board/` abstracts both behind a uniform interface across three files:

- **`mod.rs`**: screen/board guards (`compile_error!`) + the EPD/SPI/SD constants shared by both boards + common helpers + diagnostics
- **`v1.rs`**: the old board (pairs with the 2.9 / 2.7 / 4.2 screens)
- **`v2.rs`**: the new board (4.2" screen only)

Features split into two dimensions: `epd_*` selects the display driver and `board_*` selects the board; a device = an epd + board combination. The selected board file is re-exported uniformly, so callers always write `board::xxx` without knowing which board file is behind it:

```rust
// src/board/mod.rs
#[cfg(feature = "board_v1")]
pub mod v1;
#[cfg(feature = "board_v2")]
pub mod v2;
#[cfg(feature = "board_v1")]
pub use v1::*;
#[cfg(feature = "board_v2")]
pub use v2::*;
```

The build matrix:

| Feature combination | Device |
|--------------|------|
| `epd4in2,board_v2` | new board, 4.2" |
| `epd4in2,board_v1` | old board, 4.2" |
| `epd2in7,board_v1` | old board, 2.7" |
| `epd2in9,board_v1` | old board, 2.9" |

The EPD/SPI/SD pins are identical on both boards and are defined in `mod.rs`:

```rust
// src/board/mod.rs —— EPD / SPI / SD (same on both boards)
pub const EPD_BUSY_GPIO: u8 = 6;
pub const EPD_RST_GPIO: u8 = 7;
pub const EPD_DC_GPIO: u8 = 20;
pub const EPD_CS_GPIO: u8 = 3;
pub const SPI_SCLK_GPIO: u8 = 8;
pub const SPI_MOSI_GPIO: u8 = 0;
pub const SPI_MISO_GPIO: u8 = 10;
pub const SD_CS_GPIO: u8 = 5;
```

Everything that differs on the board itself lives in `v1.rs` / `v2.rs`:

| | board_v1 (old board) | board_v2 (new board) |
|------|------------------|----------------------|
| Three keys | key 1 = GPIO9 direct digital; keys 2/3 = GPIO2 ADC ladder | keys 1/2/3 all on the GPIO4 ADC ladder (doubles as deep-sleep wake pin) |
| Battery divider ADC | GPIO4 | GPIO2 |
| I2C | none | GPIO9=SDA / GPIO21=SCL, hosting PCF8563(0x51) + AHT20(0x38) |
| Power rails | dual rail: EINK=GPIO21 / SD=GPIO1, **high = power off** | single rail GPIO1, **low = power on** |
| Deep-sleep pad hold | set hold on GPIO1 (SD rail) | clear the `pad_hold` register, releasing the hold placed by the previous deep sleep |
| `HAS_PCF` / `HAS_AHT20` | false / false | true / true |

Each board file provides identically named functions, and `main` only ever calls through the `board::` prefix:

| Function | Purpose |
|------|------|
| `init_pad_hold(&mut Peripherals)` | deep-sleep pad hold (v1 sets hold / v2 releases it) |
| `init_i2c(&mut Peripherals) -> Option<I2c>` | v2 builds the I2C bus, v1 returns `None` |
| `init_power_rails(&mut Peripherals)` | builds the rail output pins and fills the sleep module's `EINK_PWER_PIN` / `SD_PWER_PIN` slots |
| `key_ladder_pin()` | the key-ladder GPIO (ADC + digital input + deep-sleep wake share one pad) |
| `key_direct_pin()` | the direct-wired digital key (v1 only) |
| `bat_pin()` | the battery divider ADC pin |
| `spawn_sensors(&Spawner)` | v2 spawns the AHT20 sampling task; v1 is a no-op |
| `print_boot_diag()` | rtc_fast self-increment diagnostic (incrementing across deep sleep = retention OK) |

Power rails are a typical example of board differences. The old board has two rails that are off when high; the new board has one rail that is on when low. Outwardly both are just "fill two slots with power-control pins":

```rust
// src/board/v1.rs —— dual rail (high = off); pulled low immediately after init to power up
pub async fn init_power_rails(p: &mut Peripherals) {
    let mut eink_pwr_ctrl = Output::new(unsafe { p.GPIO21.clone_unchecked() }, Level::High, OutputConfig::default());
    let mut sd_pwr_ctrl = Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::High, OutputConfig::default());
    eink_pwr_ctrl.set_low();
    sd_pwr_ctrl.set_low();
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}

// src/board/v2.rs —— single rail (low = on); both slots hold the same rail,
// so sleep drives it high to cut power — equivalent to the old board's two pins
pub async fn init_power_rails(p: &mut Peripherals) {
    let mut eink_pwr_ctrl = Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::Low, OutputConfig::default());
    let mut sd_pwr_ctrl = Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::Low, OutputConfig::default());
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}
```

### `Peripherals::steal()`: taking pins late

Keys and battery are initialized later in `main` — by then the EPD/SPI fields have already been moved out of `peripherals`, so there is no `&mut Peripherals` left to borrow. The board files uniformly use `Peripherals::steal()` + `clone_unchecked()` to take them again (consistent with the project-wide `clone_unchecked` convention; single-core and this code runs exactly once at startup, so it is safe):

```rust
// src/board/v2.rs
pub type KeyLadderGpio = esp_hal::peripherals::GPIO4<'static>;
pub type BatAdcGpio = esp_hal::peripherals::GPIO2<'static>;

pub fn key_ladder_pin() -> KeyLadderGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO4.clone_unchecked() }
}
pub fn bat_pin() -> BatAdcGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO2.clone_unchecked() }
}
```

The type aliases `KeyLadderGpio` / `BatAdcGpio` serve another purpose: the generic parameters of ADC statics reference them, so the types follow the board automatically when the pin changes (see Section 6).

## 3. SPI Bus Sharing: the Nature of the Problem

This project hangs two devices (EPD and SD card) off one SPI bus, each with its own chip select (CS). The naive approach: pull `CS_SD` low before talking to the card and high after; pull `CS_EPD` low before the display and high after.

The problem is concurrency: if two tasks operate nearly simultaneously, MOSI/SCK get driven by two devices and data corrupts. This matters even more in Embassy's async environment — when one task `.await`s and yields, another task may slip in and use the SPI bus; and a transfer interrupted mid-way (CS still asserted while someone else owns the bus) is even worse.

### 3.1 The solution: CsMutex + CriticalSectionDevice

This project uses `critical_section::Mutex` together with `embedded_hal_bus::spi::CriticalSectionDevice`:

```rust
// src/main.rs:260-268
use critical_section::Mutex as CsMutex;

// 1. Wrap the SPI bus in a CsMutex
let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);

// 2. Create an independent CS-gated SPI device per device
let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();

// 3. Promote to 'static lifetime
let spi_bus_sd = static_cell::make_static!(spi_bus_sd);
let spi_bus_epd = static_cell::make_static!(spi_bus_epd);
```

How it works:

```
                    SPI2 (32MHz, Mode 0)
                          │
                    CsMutex<RefCell<Spi>>
                    (critical-section mutex)
                     ╱          ╲
    CriticalSectionDevice    CriticalSectionDevice
    (CS = GPIO5, SD card)    (CS = GPIO3, display)
         │                         │
    sd_mount module           display module
```

**CsMutex** is a critical-section-based mutex. Whenever a device performs an SPI operation:
1. interrupts are disabled (enter the critical section)
2. a mutable reference to the `RefCell` is acquired
3. the device's own CS pin is pulled low
4. the SPI transfer runs
5. CS is pulled high
6. the reference is released and interrupts restored

`CriticalSectionDevice` wraps this flow into a standard `embedded_hal::SpiDevice` trait implementation — to callers it is just an ordinary SPI device, with no mutex details to worry about.

### 3.2 Why CsMutex and not an Embassy Mutex?

This is a subtle design choice:

| Lock type | Implementation | Best for |
|--------|---------|---------|
| `CriticalSectionRawMutex` | disable interrupts | short holds (microsecond-scale SPI ops) |
| `embassy::sync::Mutex` | async wait | potentially long holds (network ops, etc.) |

SPI operations are very fast (a few to a few dozen microseconds); disabling interrupts costs far less than the context switches of an async lock. And an SPI transfer cannot be interrupted by `.await` anyway (it is not async), so a critical-section lock is the right fit.

### 3.3 What is `new_no_delay`?

```rust
CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
```

`new_no_delay` means no delay object is needed. The older `CriticalSectionDevice::new()` took a `Delay` argument used to wait after CS goes low for the device to become ready. Neither this project's EPD nor its SD card needs a delay between CS and data, so `new_no_delay` simplifies the interface. This is one of the improvements in `embedded-hal-bus` 0.3.

## 4. I2C Bus Sharing and On-Board Peripherals

I2C is the second shared bus, introduced with the v2 board and wrapped in `src/i2c_bus.rs`. Like the SPI CsMutex scheme — a static `Mutex` around an `Option<I2c>`, where `None` means no device is present (old board):

```rust
// src/i2c_bus.rs
pub type I2cDev = I2c<'static, esp_hal::Blocking>;

pub static I2C: Mutex<CriticalSectionRawMutex, Option<I2cDev>> = Mutex::new(None);

pub async fn write(addr: u8, data: &[u8]) -> Result<(), ()> { ... }
pub async fn write_read(addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), ()> { ... }
```

Drivers never touch the `I2C` static directly; they go through the `write` / `write_read` helpers, with errors collapsed into `Result<(), ()>`. The bus is built by `board::init_i2c()` (GPIO9=SDA / GPIO21=SCL, `Config::default()` = 100kHz — the PCF8563's safe maximum) and hosts two devices:

- **AHT20 (0x38)**: indoor temperature/humidity sensor; `aht20::sample_task` samples periodically, started by `board::spawn_sensors`
- **PCF8563 (0x51)**: hardware RTC with a CR2032 coin-cell backup; its role is the **system clock source**

PCF8563 time is stored and read in UTC (BCD-encoded; `Clock::local` handles the time zone):

- **Writing**: after a successful NTP sync, `write_time(ts)` stores the UTC time (STOP is set first to pause the clock and prevent tearing, then cleared)
- **Reading**: every wake/reboot restores the time from it first. `read_time()` checks the VL invalid flag (returns None if the backup cell lost power) and field validity; both `ntp_worker` and reading mode's `init_clock_pcf` try it first and fall back to rtc_fast estimation on failure
- Deep-sleep drift, reading mode without WiFi, battery swaps — none of them lose the time anymore

It also handles **deep-sleep timed wake**: the countdown timer (`arm_timer_secs` / `arm_timer_minutes`, 1Hz or 1/60Hz divided, up to 255 minutes) pulls the INT pin low when it reaches zero, waking the CPU through GPIO4; longer intervals use the absolute-time alarm `arm_alarm_at` (the PCF keeps time itself, more accurate than the MCU's deep-sleep slow clock). After waking, `consume_timer_wake()` reads the TF/AF flags to tell whether this was a timed wake, then clears the flags and stops the timer — if INT kept GPIO4 low, the key network would read as a phantom key press at runtime.

## 5. Global Static State Management

In embedded Rust, hardware resources are usually globally unique — one SPI bus, one display controller, one WiFi interface. But Rust's ownership model demands a unique owner for every value, which conflicts with "globally shared".

The project uses four patterns to resolve this tension:

### 5.1 Pattern one: Embassy Mutex + Option

The most common pattern, for data shared between async tasks:

```rust
// src/storage.rs
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);

// Reading it (src/wifi.rs)
if let Some(wifi) = crate::storage::WIFI_INFO.lock().await.as_ref() {
    println!("wifi_ssid:{:?}", wifi.wifi_ssid);
}

// Writing it
WIFI_INFO.lock().await.replace(wifi);
```

Why wrap in `Option`? Because a global has no data at declaration time (the hardware isn't initialized yet), so the initial value is `None`. After initialization, `replace()` fills in the actual value.

Embassy's `Mutex` is async — `lock().await` yields while the lock is held elsewhere instead of blocking other tasks. The SPI container (CsMutex), the I2C container (`i2c_bus::I2C`), the ADC containers (`event::ADC_PER`), and the power-rail slots (`sleep::EINK_PWER_PIN`) all follow this pattern.

### 5.2 Pattern two: core::ptr safe access to static mut

For performance-sensitive single-writer multi-reader cases, such as the framebuffer:

```rust
// src/display.rs
static mut DISPLAY: Option<EpdDisplay> = None;

// Write (called once, at init only)
unsafe {
    core::ptr::addr_of_mut!(DISPLAY).write(Some(display));
}

// Read (called frequently, during rendering)
pub fn display_mut() -> Option<&'static mut EpdDisplay> {
    unsafe {
        (*core::ptr::addr_of_mut!(DISPLAY)).as_mut()
    }
}
```

Why not a Mutex? The framebuffer's access pattern is fully known:
- **Write**: once, at initialization only
- **Read**: from page code, on every render

Rendering is the most frequent operation in the system, so async Mutex overhead is unacceptable. Using `core::ptr::addr_of_mut!` is Rust's safety requirement for `static mut` access — directly dereferencing `*mut` produces warnings or even errors on newer compilers.

### 5.3 Pattern three: StaticCell + make_static!

Promotes a local variable to a `'static` lifetime:

```rust
// src/main.rs:261-262
let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);
```

The `make_static!` macro places the value in global static storage and returns `&'static mut T`. This is common with Embassy tasks — task arguments usually require `'static`, yet many values are created dynamically inside `main()`.

Internally the `static_cell` crate uses a global `OnceCell` to guarantee each value is initialized exactly once.

### 5.4 Pattern four: RTC fast memory

For variables that must survive deep sleep:

```rust
// src/display.rs
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES: u32 = 0;
```

The ESP32-C3's RTC fast memory is a special SRAM region that stays powered during deep sleep. Variables placed there keep their values across wake-up.

`#[ram(unstable(rtc_fast))]` is the esp-hal v1.x spelling (older versions used `#[ram(rtc_fast)]`) and requires the `unstable` feature.

The project has several RTC variables: render count (`RENDER_TIMES`), cold-boot clear flag (`NEED_CLEAR`), battery percentage (`LAST_BATTERY_PERCENT`), sleep timestamp (`WHEN_SLEEP_RTC_MS`), boot sequence number (`board::RTC_FAST_BOOT_SEQ`, for diagnostics) — all of which need their state restored after wake-up.

Know rtc_fast's boundaries: it survives **deep sleep only** — not power loss, not a software reset; and on the new board an IO (RTCIO) wake can also lose it. So critical state keeps an NVS backup — e.g. the "was reading" `READING_MODE` flag lives in NVS and is restored from there on wake, with rtc_fast as merely the fast path.

### The four patterns compared

| Pattern | Safety | Performance | Best for |
|------|--------|------|---------|
| Embassy Mutex + Option | safe (compile-time + runtime) | lower (async lock) | data shared across tasks |
| core::ptr + static mut | unsafe | highest | single-writer, high-frequency access |
| StaticCell | safe | one-shot | promoting locals to `'static` |
| RTC memory | unsafe | highest | surviving deep sleep |

## 6. ADC: Analog Input and Multi-Key Detection

The three keys use an ADC resistor ladder: pressing different keys produces different midpoint voltages, hence different ADC readings. The pin comes from the board abstraction — on v2 all three keys sit on the GPIO4 ladder, on v1 only keys 2/3 sit on the GPIO2 ladder (key 1 is wired directly to GPIO9):

```rust
// src/main.rs:199-212
let mut adc1_config = AdcConfig::new();
// Key ladder: v2=GPIO4 / v1=GPIO2
let adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
    crate::board::key_ladder_pin(), Attenuation::_11dB,
);
// Battery divider: v2=GPIO2 / v1=GPIO4
let bat_adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
    crate::board::bat_pin(), Attenuation::_11dB,
);

let adc1 = Adc::new(peripherals.ADC1, adc1_config);
event::ADC_PER.lock().await.replace(adc1);
event::ADC_PIN.lock().await.replace(adc1_pin);
```

esp-hal v1.x's ADC type system is strict:

```rust
// src/event.rs
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<
    AdcPin<crate::board::KeyLadderGpio, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>
>> = Mutex::new(None);

pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<
    Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>
>> = Mutex::new(None);
```

The generic parameters go all the way down to the concrete pin type, the ADC controller type, and the calibration method. The key is that the pin type references the board-level alias `crate::board::KeyLadderGpio` (v2=GPIO4 / v1=GPIO2) — switching boards only touches the board file, and the event module's statics follow automatically. Older versions offered no such precision; it buys real compile-time type safety.

### 6.1 Key identification: median filter + per-board thresholds

`judge_adc_num()` samples 12 times and takes the median (rejects transition glitches), then applies per-board thresholds:

```rust
// src/event.rs
#[cfg(feature = "board_v2")]
{
    // v2 (GPIO4 ladder, all three keys on it, measured ≈25/409/645):
    // <220→key 1; 220~520→key 2; 520~900→key 3; higher = glitch, ignore
    if mid < 220 { 1 } else if mid < 520 { 2 } else if mid < 900 { 3 } else { 0 }
}
#[cfg(feature = "board_v1")]
{
    // v1 (GPIO2 ladder, only keys 2/3 on it; key 1 uses GPIO9 direct and never enters
    // this function): <200→key 2; 200~1000→key 3; >1000 glitch, ignore
    if mid < 200 { 2 } else if mid > 1000 { 0 } else { 3 }
}
```

### 6.2 The two board variants of the scan task

`event::run` likewise has one variant per board. On v2 all three keys share GPIO4, and pressing any of them pulls the pin to a digital low (the measured ladder voltages are all below the digital low threshold) — so a falling edge detects "a key was pressed" while the ADC median tells "which one"; sampling happens immediately after the edge, because a quick tap can release within a few tens of ms:

```rust
// src/event.rs (v2)
#[embassy_executor::task]
pub async fn run(key2: crate::board::KeyLadderGpio) {
    let mut key_all = Input::new(key2, InputConfig::default().with_pull(Pull::Up));
    loop {
        key_all.wait_for_falling_edge().await;
        let num = judge_adc_num().await;      // 0 = glitch, ignore
        if num == 0 { Timer::after_millis(10).await; continue; }
        key_detection(&mut key_all, num).await;
        refresh_active_time().await;
        crate::sleep::mark_user_touch();
        Timer::after_millis(10).await;
    }
}
```

v1 is dual-path: the GPIO9 direct key (key 1) and the GPIO2 ladder (keys 2/3) wait for falling edges simultaneously via `embassy_futures::select`:

```rust
// src/event.rs (v1, excerpt)
pub async fn run(key1: GPIO9<'static>, key2: KeyLadderGpio) {
    loop {
        match select(key1.wait_for_falling_edge(), key2.wait_for_falling_edge()).await {
            Either::First(_) => key_detection(&mut key1, 1).await,
            Either::Second(_) => { /* judge_adc_num() → key_detection(&mut key2, num) */ }
        }
    }
}
```

`key_detection` is the shared key state machine: of 100 samples, more than 80 low reads mean "pressed"; held over 500ms emits `KeyLongStart`, then one `KeyLongIng` every 100ms; release emits `KeyLongEnd`; a short press waits up to 400ms for a second press — one arrives emits `KeyDouble` (gated by the `ENABLE_DOUBLE` switch), otherwise `KeyShort`.

### 6.3 The event queue: decoupling scanning from dispatch

In the earlier implementation, `toggle_event` walked the listener callbacks right on the scan path — one slow callback blocked scanning and keys pressed meanwhile were lost. It is now a producer-consumer pair:

```rust
// src/event.rs
/// Key event queue: capacity 8. Producer = toggle_event (scan/state machine),
/// consumer = the dispatch task.
static EVENT_QUEUE: Mutex<CriticalSectionRawMutex, Vec<EventType, 8>> = Mutex::new(Vec::new());
static EVENT_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
```

- **Producer** `toggle_event` only enqueues: consecutive `KeyLongIng` for the same key merge at the tail (no piling up), a full queue drops the oldest to keep the newest, then `EVENT_READY.signal()` fires
- **Consumer**: the `dispatch` task wakes on the signal and dispatches FIFO, serially invoking each matching listener callback

The cost is one constraint: dispatch holds the `LISTENER` lock and callbacks run serially — **callbacks must not call `on` / `clear` / `on_fixed` to rebind events** (it would deadlock); page-switch rebinding keeps the existing page `run`-loop convention.

## 7. RTC: Real-Time Clock and Sleep Control

```rust
// src/main.rs:135-136
let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
crate::sleep::RTC_MANGE.lock().await.replace(rtc);
```

The RTC (Real-Time Clock) is the ESP32-C3's low-power peripheral; it runs independently of the CPU and keeps counting through deep sleep. In this project the ESP RTC:
- records when the system went to sleep, so wake-up can rebuild the system time from the difference (the rtc_fast estimation path)
- configures wake sources (timer, GPIO — the key-ladder pin is registered as an RTC IO wake-on-low source)
- controls deep sleep

The `LPWR` (Low Power) peripheral comes straight from `Peripherals` in esp-hal v1.x, replacing the older `RTC_CNTL`.

The v2 board adds a second clock: the external PCF8563 hardware RTC (see Section 4). The division of labor — **system-time restoration prefers the PCF8563** (written after NTP sync, kept alive by the CR2032), with rtc_fast estimation demoted to fallback; **timed wake prefers the PCF8563's INT pin** (its timekeeping is precise), with the ESP RTC timer as the fallback when arming fails. The old board has no PCF8563, so both roles fall back to the ESP RTC.

## Summary

This article covered the hardware abstraction in depth:

| Topic | Key point |
|--------|------|
| Peripheral ownership | each `Peripherals` field moves exactly once; escape hatch is `Peripherals::steal()` |
| GPIO new API | `Output::new(pin, level, OutputConfig)` adds a config struct |
| Board abstraction | `epd_*` × `board_*` feature dimensions; board files expose a uniform API; rails/I2C/keys switch per board |
| SPI config | `SpiConfig` builder pattern, `Rate::from_mhz()` frequency |
| SPI sharing | `CsMutex` + `CriticalSectionDevice`, critical-section mutual exclusion |
| I2C sharing | `i2c_bus` static Mutex wrapper hosting PCF8563(0x51) / AHT20(0x38) |
| Global state | four patterns: Embassy Mutex / core::ptr / StaticCell / RTC memory |
| ADC | strict generics + board-level type aliases; 12-sample median; per-board thresholds identify the key |
| Event queue | `toggle_event` enqueues + `dispatch` serially delivers; keys are not lost during slow callbacks |
| RTC | the `LPWR` peripheral controls deep sleep; on v2 the external PCF8563 handles clock restore and timed wake |

The core idea: **use Rust's type system to enforce hardware-access safety at compile time (guards, ownership, type aliases), and runtime locks to coordinate concurrency where necessary (critical sections, async Mutexes)**.

In the next article we enter the world of e-ink displays and look at how the rendering architecture is designed.

---

> Previous: [Part 1: Project Overview](01-project-overview.md) · Next: [Part 3: E-Ink Display Driver and Rendering Architecture](../03-电子墨水屏驱动与渲染架构.md)
