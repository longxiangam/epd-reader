# Part 1: Project Overview — Building an ESP32-C3 Embedded Rust Development Environment from Scratch

> 中文版: [../01-项目全景.md](../01-项目全景.md)
>
> Building an ESP32-C3 E-Ink Reader in Rust · Article Series

## Introduction

This is an e-ink reader running on an ESP32-C3, written in Rust with `#![no_std]` — no standard library linkage, no operating system; the program runs on bare metal. The device has WiFi and can do all of this: read TXT e-books (with vector TTF font rendering), view weather and temperature trends, browse the Chinese lunar calendar with holidays (including adjusted workdays), check Shanghai/Shenzhen stock intraday/K-line/order-book data, view BMP images, plus QR-code provisioning from a phone, IP-based geolocation, scheduled refresh, and deep-sleep power saving.

There are two hardware board generations, switched at compile time via features: `board_v1` (the old board: no I2C, dual power rails, key 1 direct-wired) and `board_v2` (the new board: I2C bus with an AHT20 temperature/humidity sensor and a PCF8563 hardware clock, single power rail, all three keys on an ADC resistor ladder). Both boards share the same display driver and SPI/SD pins.

This first article covers the overall structure and how to set up the development environment. Later articles break the system down by subsystem: SPI bus sharing, e-ink display driver and rendering, the Embassy async runtime and events, WiFi and HTTP, Flash persistence and the SD card, low power, UI components and pages, error handling, e-book reading (vector/bitmap fonts and dynamic pagination), and firmware image building and flashing.

## 1. Starting from `#![no_std]`

When you write Rust on a PC, you take `String`, `Vec`, and `println!` for granted — they all come from the Rust standard library (`std`). On a microcontroller, however, there is no OS, no filesystem, no standard I/O — most of the standard library is unavailable.

So the first line of any embedded Rust program is always:

```rust
#![no_std]
```

This declaration tells the compiler: do not link the standard library; we only use `core` (Rust's minimal core library, which does not depend on an OS).

Next, an embedded program has no conventional `main()` entry point either — the C runtime (crt0) won't call it for us. So we also need:

```rust
#![no_main]
```

So who provides the program entry? On the ESP32, `esp-rtos` does:

```rust
// src/main.rs
#![no_std]
#![no_main]

esp_bootloader_esp_idf::esp_app_desc!();  // Generate application description

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // Execution starts here
    // Note the return type -> !: it never returns
}
```

`#[esp_rtos::main]` is a proc macro that wraps the `main` function into the proper ESP32 entry point and sets up the Embassy async executor. `-> !` means the function never returns — in the embedded world, a program is an infinite loop.

### Unstable nightly features

Because the embedded Rust ecosystem (esp-hal, Embassy, etc.) depends on some not-yet-stabilized Rust features, we need the nightly compiler and must declare those features at the top of the file:

```rust
// src/main.rs
#![feature(impl_trait_in_assoc_type)]   // impl Trait in trait associated types
#![feature(type_alias_impl_trait)]      // impl Trait in type aliases
#![feature(generic_const_exprs)]        // generic constant expressions
```

These features are widely used in embassy-net, embedded-graphics, and other libraries — standard equipment for embedded Rust development today.

## 2. ESP32-C3 and RISC-V

The ESP32-C3 is Espressif's low-cost, low-power microcontroller built on the **RISC-V 32-bit instruction set** (specifically `riscv32imc`), clocked at 160MHz, with 400KB SRAM and 4MB Flash on board.

Why choose it:
- **Cheap** — module units cost only a dozen or so yuan
- **Has WiFi** — standard across the ESP32 family
- **RISC-V architecture** — no ARM licensing cost, and friendlier for learning
- **Mature Rust ecosystem** — `esp-rs` treats the ESP32-C3 as a first-class citizen

Let's clear up a common misconception: the ESP32-C3 has no MMU and cannot run Linux; but that doesn't mean `no_std` is the only option. Rust has two paths on this chip — one is **std mode** based on ESP-IDF (the `riscv32imc-esp-espidf` target, where ESP-IDF provides task scheduling, WiFi, and other system services, and the standard library works), and the other is **esp-hal's bare-metal mode**.

This project takes the second path: esp-hal to drive hardware directly, Embassy for async tasks, no ESP-IDF dependency — hence `#![no_std]`. The benefit is more direct control over hardware, memory, and power; the cost is that the standard library conveniences (`Vec`/`String`/`println!`, etc.) must be replaced with `alloc` + a heap allocator or `heapless` collections.

### The target triple

In Rust cross-compilation, the "target triple" identifies the compilation target platform. For the ESP32-C3 it is:

```
riscv32imc-unknown-none-elf
```

Breaking it down:
- `riscv32imc`: RISC-V 32-bit with integer (I), multiplication (M), and compressed (C) extensions
- `unknown`: unknown vendor
- `none`: no operating system
- `elf`: ELF output format

This triple runs through the entire project configuration.

## 3. Toolchain Configuration

### 3.1 rust-toolchain.toml

A Rust project declares its toolchain in `rust-toolchain.toml`:

```toml
# rust-toolchain.toml
[toolchain]
targets = ["riscv32imc-unknown-none-elf"]
```

Only the target is specified, with no pinned nightly version. In your own projects you can pin a specific nightly date (e.g. `channel = "nightly-2024-09-19"`), but esp-hal v1.x is compatible with recent nightlies, so pinning is usually unnecessary.

### 3.2 Installing the toolchain

Before developing, install the target platform and flashing tools:

```bash
# Add the RISC-V target
rustup target add riscv32imc-unknown-none-elf

# Install espflash (build, flash, monitor)
cargo install espflash

# (Optional) Install probe-rs for debugger-based flashing
cargo install probe-rs
```

### 3.3 .cargo/config.toml

Cargo's build configuration defines how to compile and run:

```toml
# .cargo/config.toml
[target.riscv32imc-unknown-none-elf]
runner = "espflash flash --chip esp32c3 --baud 115200 --monitor --partition-table ./partitions.csv"

[build]
rustflags = [
  "-C", "link-arg=-Tlinkall.x",        # Memory layout linker script
  "-C", "force-frame-pointers",          # Keep frame pointers for backtrace debugging
]
target = "riscv32imc-unknown-none-elf"

[env]
CC = "riscv32-esp-elf-gcc"               # C compiler (needed by the linker script)
AR = "riscv32-esp-elf-ar"

[net]
git-fetch-with-cli = true                # Fetch git dependencies via system git (this project has self-maintained forks)
```

Key points:
- **runner**: the command `cargo run` executes. `espflash flash` builds, flashes, and opens the serial monitor. `--partition-table ./partitions.csv` tells the flasher to use our custom partition table
- **linkall.x**: the ESP32 memory-layout linker script defining RAM/Flash address ranges
- **force-frame-pointers**: forces frame pointers so `esp-backtrace` can print the call stack on crash

With this configuration, one command builds and flashes the device:

```bash
cargo run --release --features "epd4in2,board_v2"
```

> `--release` is mandatory: the firmware no longer fits in the 3MB factory partition under default debug settings; `[profile.release] opt-level = "s"` makes it fit. The feature argument combines two dimensions — screen type plus board type; see Section 6.

## 4. The Dependency Ecosystem

The ESP32 embedded Rust ecosystem is layered. Here is the project's `Cargo.toml`:

```toml
# Cargo.toml (excerpt)
[package]
name = "epd-reader"
edition = "2024"

[dependencies]
# ── Official ESP ecosystem ──
esp-hal = { version = "1.1.1", features = ["esp32c3", "unstable", "log-04"] }
esp-radio = { version = "0.18.0", features = ["esp32c3", "wifi", "log-04", "unstable"] }
esp-rtos = { version = "0.3.0", features = ["esp32c3", "embassy", "esp-radio", "log-04"] }
esp-alloc = { version = "0.10.0", features = ["esp32c3", "global-allocator"] }
esp-storage = { version = "0.9.0", features = ["esp32c3"] }
esp-bootloader-esp-idf = { version = "0.5.0", features = ["esp32c3"] }
esp-backtrace = { version = "0.19.0", features = ["esp32c3", "println"] }
esp-println = { version = "0.17.0", features = ["esp32c3", "log-04"] }

# ── Embassy async framework ──
embassy-executor = { version = "0.10.0", features = ["nightly"] }
embassy-time = "0.5.0"
embassy-net = { version = "0.9.0", features = ["dhcpv4", "medium-ethernet", "tcp", "dns", "udp"] }
embassy-sync = "0.8"

# ── Graphics ──
embedded-graphics = "0.8"
u8g2-fonts = { version = "0.7.1", features = ["embedded_graphics_textstyle"] }
# Professional coverage rasterizer (no_std + libm), used for TTF vector glyph rendering
ab_glyph_rasterizer = { version = "0.1.10", default-features = false, features = ["libm"] }

# ── Networking ──
reqwless = { version = "0.14.0", default-features = false }
embedded-tls = { version = "0.18", default-features = false }
no-std-net = "0.6"
sntpc = { version = "0.3", features = ["async"] }

# ── Storage ──
# Self-maintained fork: the lfn-create branch adds LFN (long filename) file creation
embedded-sdmmc = { git = "https://github.com/longxiangam/embedded-sdmmc-rs", branch = "lfn-create" }
```

Beyond the crates.io dependencies, the project pulls in two more self-maintained forks as git dependencies: `epd-waveshare` (display driver patches) and `mini-json`. Together with `embedded-sdmmc` above, all local modifications stay in our own repos, making upstream tracking easy.

These dependencies form layers:

```
┌─────────────────────────────────────┐
│    Application (Pages/Widgets)      │  ← our code
├─────────────────────────────────────┤
│   Services (display, wifi, etc.)    │  ← our code
├─────────────────────────────────────┤
│     Embassy async framework         │  ← embassy-executor/net/sync
├─────────────────────────────────────┤
│     ESP HAL + drivers               │  ← esp-hal / esp-radio / esp-alloc
├─────────────────────────────────────┤
│     Hardware (ESP32-C3)             │
└─────────────────────────────────────┘
```

Every ESP-related crate needs `features = ["esp32c3"]`, because the ESP32 family has many models (C3, C6, S2, S3, etc.) with different register definitions and peripheral sets.

### What feature flags do

The `unstable` feature of `esp-hal` deserves special mention. esp-hal v1.x marks some APIs "unstable", requiring explicit opt-in:

```toml
esp-hal = { version = "1.1.1", features = ["esp32c3", "unstable"] }
```

These APIs may change in future versions, but are fully functional in the current one. For a real project, enabling `unstable` is necessary — many critical features (such as the RTC memory attribute `#[ram(unstable(rtc_fast))]`) sit behind it.

## 5. The Flash Partition Table

The ESP32's 4MB Flash is not one big free-for-all; it is divided into partitions with distinct purposes, defined in `partitions.csv`:

```csv
# partitions.csv
# Name,   Type, SubType,  Offset,   Size, Flags
nvs,      data, nvs,      ,         0x6000,
phy_init, data, phy,      ,         0x1000,
factory,  app,  factory,  ,         3M,
storage,  data, fat,      ,         128K,
```

Diagram:

```
0x9000        0x9000+24KB  0x10000      0x310000      0x330000
  ├─────────────┤            ├──────────────┤─────────────┤
  │     NVS      │  phy_init │   factory    │   storage   │
  │   (24KB)     │  (4KB)    │    (3MB)     │   (128KB)   │
  │ config/cache │           │ applications │  FAT files  │
  └─────────────┘            └──────────────┘─────────────┘
```

What each partition does:

| Partition | Size | Purpose |
|------|------|------|
| **nvs** | 24KB | Non-volatile storage. WiFi config, weather cache, settings, error logs, the reading-mode flag all live here |
| **phy_init** | 4KB | WiFi PHY calibration data, managed automatically by the ESP-IDF bootloader |
| **factory** | 3MB | The compiled firmware (our application); the largest consumer |
| **storage** | 128KB | FAT filesystem partition, used for standby wallpaper images |

NVS reads and writes go through `esp-storage`; a later article details its storage layout.

## 6. Feature Flags: Two Dimensions — Screen and Board

The project supports multiple screens and two board generations. Cargo feature flags combine at compile time into a concrete device. Features split into two dimensions: `epd_*` selects the display driver (one of three), `board_*` selects the board (one of two); **a device = an epd + board combination**:

```toml
# Cargo.toml
[features]
# ── Screen (choose one of three) ──
epd2in9 = []           # 2.9" display (296×128) — weather/calendar pages adapted for landscape; layout not finely tuned
epd2in7 = []           # 2.7" display (264×176)
epd4in2 = []           # 4.2" display (400×300)
# ── Board (choose one of two) ──
board_v1 = []          # Old board: no I2C, dual power rails, key 1 = GPIO9 direct
board_v2 = []          # New board: I2C (AHT20+PCF8563), single power rail, 4.2" only
enable_debug = []      # Enter the debug page at boot if Flash holds error logs
weather-openmeteo = [] # Switch weather source to Open-Meteo (default: Xinzhi)
ttf_spike = ["dep:ab_glyph"]  # TTF vector glyph rendering benchmark (optional dep ab_glyph)
```

Both dimensions have compile-time guards (`src/board/mod.rs`): a wrong combination fails to compile instead of misbehaving at runtime:

```rust
// Screen is one-of-three, board is one-of-two, and board_v2 pairs only with the 4.2" screen
#[cfg(all(feature = "epd2in9", feature = "epd4in2"))]
compile_error!("屏型 feature 只能选一个：epd2in9 / epd2in7 / epd4in2");
#[cfg(all(feature = "board_v1", feature = "board_v2"))]
compile_error!("板型 feature 只能选一个：board_v1 / board_v2");
#[cfg(all(feature = "board_v2", any(feature = "epd2in9", feature = "epd2in7")))]
compile_error!("board_v2 板只能配 epd4in2 屏");
```

Code adapts to each screen via `#[cfg(...)]` conditional compilation:

```rust
// src/display.rs
#[cfg(feature = "epd2in9")]
pub type EpdDisplay = Display2in9;          // 296×128

#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;          // 400×300
```

UI layout also differs per screen, for example text line count:

```rust
// src/epd2in9_txt.rs
#[cfg(feature = "epd2in9")]
const LINES_NUM: u32 = 7;     // 2.9" screen shows 7 text lines

#[cfg(feature = "epd2in7")]
const LINES_NUM: u32 = 9;     // 2.7" screen shows 9 text lines

#[cfg(feature = "epd4in2")]
const LINES_NUM: u32 = 22;    // 4.2" screen shows 22 text lines
```

Select via the `--features` argument at build time (always with `--release`):

```bash
cargo run --release --features "epd4in2,board_v2"   # new board (400×300)
cargo run --release --features "epd4in2,board_v1"   # old board, 4.2" (400×300)
cargo run --release --features "epd2in7,board_v1"   # old board, 2.7" (264×176)
cargo run --release --features "epd2in9,board_v1"   # old board, 2.9" (296×128; weather/calendar layout not finely tuned)
cargo run --release --features "epd4in2,board_v2,enable_debug"  # with debug mode
```

> The board dimension decides keys, battery ADC, I2C peripherals, and power rails (see the board abstraction in Part 2); the screen dimension only decides the display driver and layout. Both boards share the same EPD/SPI/SD pins.

## 7. Program Entry and the Initialization Flow

Now for the full initialization flow of `main()` — the project's "starting engine":

```rust
// src/main.rs (excerpt)
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // 1. Put the heap in a dedicated dram2 segment (~64KB of DRAM released by the
    //    bootloader); it does not touch main DRAM
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(DRAM2_HEAP) as *mut u8;
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            heap_ptr, 65280,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }

    // 2. HAL init
    let config = HalConfig::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = hal_init(config);

    // 3. Board init (implementation switches on board_v1 / board_v2)
    crate::board::init_pad_hold(&mut peripherals);          // deep-sleep pad hold (v2 releases the previous hold)
    let i2c = crate::board::init_i2c(&mut peripherals);     // v2: I2C bus; v1: None
    crate::board::sd_miso_pullup(&mut peripherals);         // MISO pull-up, more reliable SD init
    crate::board::init_power_rails(&mut peripherals).await; // power rails (display/SD supply control)

    // 4. Configure the Embassy timer
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // 5. Init the RTC
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    crate::sleep::RTC_MANGE.lock().await.replace(rtc);

    // 6. Load Flash storage
    crate::storage::enter_process().await;

    // 7. Wake classification: PCF8563 timer wake / ESP RTC timer fallback / button wake
    let timed_wake = crate::pcf8563::consume_timer_wake().await
        || matches!(wake_reason, esp_hal::system::SleepSource::Timer);
    crate::sleep::set_timed_wake(timed_wake, ...);

    // 8. Configure GPIO pins (both boards share the same EPD/SPI/SD pins)
    let epd_busy = peripherals.GPIO6;
    let epd_cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
    // ... more pins

    // 9. Init the SPI bus
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(32))
            .with_mode(Mode::_0),
    ).unwrap()
    .with_sck(epd_sclk)
    .with_miso(epd_miso)
    .with_mosi(epd_mosi);

    // 10. Register the I2C bus (v2 only; used by PCF8563/AHT20)
    if let Some(i2c) = i2c {
        crate::i2c_bus::I2C.lock().await.replace(i2c);
    }

    // 11. ADC: key ladder + battery divider (pins come from board, per board type)
    let adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
        crate::board::key_ladder_pin(), Attenuation::_11dB);
    let bat_adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
        crate::board::bat_pin(), Attenuation::_11dB);

    // 12. Start key scanning / event dispatch / on-board sensor tasks
    spawner.spawn(event::run(...)).unwrap();        // v2: single ladder; v1: direct key + ladder
    spawner.spawn(event::dispatch()).unwrap();      // serial event dispatch
    crate::board::spawn_sensors(&spawner);          // v2: AHT20 sampling

    // 13. Share the SPI bus between EPD and SD card
    let shared_spi = CsMutex::new(RefCell::new(spi));
    let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();
    let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();

    // 14. Start the display render task
    spawner.spawn(display::render(spi_bus_epd, epd_busy_in, epd_rst_out, epd_dc_out)).unwrap();

    // 15. Mount the SD card
    let sdcard = SdCard::new_with_options(spi_bus_sd, embassy_time::Delay, ...);
    let volume_mgr = VolumeManager::new(sdcard, TimeSource);
    crate::sd_mount::SD_MOUNT.lock().await.replace(SdMount::new(volume_mgr));

    // 16. Register the key pin as a deep-sleep wake source (RTC IO, wake on low)
    let rtc_io = static_cell::make_static!(crate::board::key_ladder_pin());
    add_rtcio(rtc_io, WakeupLevel::Low).await;

    // 17. Reading-mode branch: last session was reading (NVS flag) → skip WiFi, give the heap to TTF
    if crate::storage::read_sleep_page() == Some(SleepPageStorage::PAGE_READING) {
        crate::worldtime::init_clock_pcf().await;   // reading mode restores the clock from the PCF8563
        spawner.spawn(crate::pages::reading_task(spawner.clone())).unwrap();
        loop { Timer::after(Duration::from_secs(5)).await; }  // main never proceeds
    }

    // 18. AP provisioning or STA connect, depending on whether WiFi is configured
    if need_ap {
        crate::wifi::start_wifi_ap(&spawner, rng, peripherals.WIFI).await;
        // run the provisioning page in a loop
    } else {
        spawner.spawn(crate::battery::test_bat_adc()).unwrap();
        spawner.spawn(crate::worldtime::ntp_worker()).unwrap();
        spawner.spawn(pages::main_task(spawner.clone())).unwrap();
        crate::wifi::connect_wifi(&spawner, rng, peripherals.WIFI).await;
    }

    // 19. Main loop: show the time + auto-sleep detection
    loop {
        to_sleep_tips(
            Duration::from_secs(0),
            crate::sleep::idle_for(Duration::from_secs(180)),
            true,
        ).await;
        Timer::after(Duration::from_secs(5)).await;
    }
}
```

The flow as a diagram:

```
esp_alloc::HEAP.add_region(dram2, 64KB)  ← heap in its own segment; main DRAM all stack
        │
   hal_init(CpuClock::max())     ← takes ownership of all peripherals
        │
   board::init_pad_hold / init_i2c / sd_miso_pullup / init_power_rails
        │                         ← board init (v1/v2 implementations)
   esp_rtos::start(timg0, sw_int)  ← starts the Embassy scheduler
        │
   Rtc::new()                     ← real-time clock
        │
   storage::enter_process()       ← load config from Flash
        │
   consume_timer_wake()           ← wake classification (PCF timer / ESP RTC / button)
        │
   GPIO / SPI / ADC init          ← hardware pin configuration
        │
   ┌─ spawn(event::run)           ← key scanning task (arguments depend on board)
   ├─ spawn(event::dispatch)      ← event dispatch task
   ├─ board::spawn_sensors        ← v2: AHT20 temp/humidity
   │
   ├─ CsMutex + CriticalSectionDevice ← SPI sharing
   ├─ spawn(display::render)      ← display task (independent Embassy task)
   ├─ SdCard + VolumeManager      ← SD card filesystem
   ├─ add_rtcio(key_ladder)       ← key pin = deep-sleep wake source
   │
   ├─ reading-mode flag (NVS)?
   │    ├── Yes → init_clock_pcf → spawn(reading_task) → never returns
   │    └── No  ↓
   ├─ WiFi configured?
   │    ├── No  → AP provisioning mode (provisioning page loop)
   │    └── Yes → spawn(battery) → spawn(ntp_worker)
   │            → spawn(main_task) ← main UI task
   │            → connect_wifi()
   │
   └─ Main loop: clock display + sleep detection
```

### A key concept: peripheral ownership

Look at this line:

```rust
let peripherals = hal_init(config);
```

`peripherals` is a struct holding every ESP32-C3 hardware peripheral (GPIO, SPI, ADC, WiFi, etc.). In esp-hal's design, **each peripheral can be taken exactly once** — guaranteed at compile time by Rust's type system.

When you write:

```rust
let epd_busy = peripherals.GPIO6;
```

`GPIO6` is "moved" out of `peripherals`, and later code can no longer access it. Exclusive hardware access is thus enforced by the compiler — once ownership moves, the code simply doesn't compile.

There is one escape hatch: pins initialized "late" (keys, battery), taken only after the EPD/SPI fields have been moved out of `peripherals`. Board code uses `Peripherals::steal()` to take them again — safe here because the core is single-core and this code runs exactly once at boot (see the board abstraction in Part 2).

## 8. The Memory Allocator

An embedded environment has no OS-provided `malloc/free`; memory must be managed yourself. This project uses `esp-alloc` — but the heap does not sit in main DRAM. It goes into `dram2`, a ~64KB DRAM region the bootloader releases and that would otherwise sit idle:

```rust
// src/main.rs
/// Heap buffer in a dedicated dram2 segment. Benefits: main DRAM is left entirely
/// to the stack (TTF rendering has a deep async call chain; a 64KB heap in main
/// DRAM once caused a stack overflow), while the heap is still big enough
/// (WiFi ~60KB / TtfWs 44KB).
#[unsafe(link_section = ".dram2_uninit")]
static mut DRAM2_HEAP: core::mem::MaybeUninit<[u8; 65280]> = core::mem::MaybeUninit::uninit();

// At the top of main(): register this memory as a heap region
unsafe {
    let heap_ptr = core::ptr::addr_of_mut!(DRAM2_HEAP) as *mut u8;
    esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
        heap_ptr,
        65280,
        esp_alloc::MemoryCapability::Internal.into(),
    ));
}
```

This memory layout buys three things:
1. **Main DRAM goes entirely to the stack** — TTF vector glyph rendering has a deep async call chain; when the heap and stack shared main DRAM, the stack once overflowed
2. **A 64KB heap**, enough for the WiFi stack (~60KB) and the TTF glyph workspace (44KB)
3. The stack-overflow watchdog (a watchpoint at `_stack_end`) keeps working

> Large HTTP request/response buffers (TCP RX/TX 4KB×2 + TLS 8KB×2 + an 8KB response body, ~33KB total) live in `.bss` (static memory), serialized for exclusive use by a WiFi request lock, so they never burden the heap.

```rust
extern crate alloc;        // bring in the alloc crate
use alloc::string::String;  // String is now available
use alloc::vec::Vec;        // Vec is now available
```

The ESP32-C3 has about 400KB SRAM in total: 64KB of dram2 goes to the heap, and the remaining main DRAM is shared by the stack, static variables (including the network buffers placed in .bss), and Embassy tasks. In such a constrained environment, allocation needs constant attention — heap fragmentation can make allocations fail. So on critical paths the project prefers fixed-size `heapless` collections:

```rust
use heapless::String as HString;
let ssid: HString<32> = HString::new();  // fixed 32 bytes, allocated on the stack, never fragments
```

## 9. Edition 2024

The project uses Rust Edition 2024:

```toml
edition = "2024"
```

This is the latest Rust edition, with several style-affecting changes:
- **Enum variants use PascalCase**: `Mode::_0` instead of `Mode::Mode0`
- **`impl Trait` in more positions**: thanks to the nightly features
- **Adjusted lifetime inference rules**: some cases need more explicit lifetime annotations

## Summary

This article established the project's overall picture:

| Concept | Key point |
|------|------|
| `#![no_std]` | Embedded Rust skips the standard library; only `core` |
| `#![no_main]` | The `esp_rtos::main` macro provides the entry point |
| Target platform | `riscv32imc-unknown-none-elf`, bare-metal RISC-V |
| Toolchain | nightly Rust + espflash flashing |
| Partition table | NVS(24KB) + Factory(3MB) + Storage(128KB) |
| Heap | esp-alloc, heap in a dedicated dram2 segment (~64KB); main DRAM all stack |
| Feature flags | Two dimensions — screen (`epd*`) and board (`board_*`) — composed at compile time |
| Board abstraction | `board_v1` / `board_v2` each define pins and power rails behind a uniform API |
| Peripheral ownership | Compile-time prevention of peripheral reuse; escape hatch is `Peripherals::steal()` |

In the next article we dive into the hardware abstraction layer: how the two board generations switch behind a uniform board abstraction, how the SPI bus is shared safely between the e-ink display and the SD card, and the patterns used for global state management in embedded Rust.

---

> Next: [Part 2: Hardware Abstraction and SPI Bus Sharing](02-hardware-abstraction-and-spi-bus.md)
