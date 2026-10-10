# EPD Reader Design Document

> 中文 | [DESIGN.md](DESIGN.md)

> An ESP32-C3 e-paper reader built with Rust (esp-hal v1.x)

---

## Table of Contents

1. [Project Overview](#1-project-overview)
2. [System Architecture](#2-system-architecture)
3. [Build System & Configuration](#3-build-system--configuration)
4. [Module Structure](#4-module-structure)
5. [Core Modules](#5-core-modules)
6. [Data Model Layer](#6-data-model-layer)
7. [UI Widget Layer](#7-ui-widget-layer)
8. [Page System](#8-page-system)
9. [Key Design Patterns](#9-key-design-patterns)
10. [Data Flow](#10-data-flow)
11. [Memory Management](#11-memory-management)
12. [Async Architecture](#12-async-architecture)
13. [Pin Assignment](#13-pin-assignment)
14. [Appendix: esp-hal Upgrade Notes](#14-appendix-esp-hal-upgrade-notes)

---

## 1. Project Overview

### 1.1 Introduction

epd-reader is an embedded e-paper display (E-Paper Display) reader running on the **ESP32-C3**, written in Rust. It implements e-book reading, weather queries, the lunar calendar, stock quotes, image viewing, WiFi provisioning, and more — a fully functional embedded application.

The project is built on the **esp-hal v1.x** ecosystem, using the latest `esp-radio` wireless driver, `esp-rtos` async runtime integration, and `esp-alloc` memory manager — state-of-the-art Rust embedded ESP32 practice as of 2025.

The hardware uses a **board abstraction**: a two-dimensional feature selection — `epd_*` picks the display driver (epd2in9 / epd2in7 / epd4in2), `board_*` picks the board hardware (board_v1 legacy / board_v2 new v22). A device = epd + board combination, with all IO assignments fixed at compile time.

### 1.2 Feature List

| Feature | Description |
|---------|-------------|
| E-book reading | TXT files; TTF vector fonts with dynamic pagination + built-in bitmap fonts; bookmarks, two-level book list, typography settings (font size / line spacing / stroke bolding / rotation / key swap), progress display |
| Weather | Seniverse / Open-Meteo dual data sources, 5-day forecast + temperature chart + indoor temperature & humidity (onboard AHT20 on v2) |
| Auto location | IP-based reverse lookup of city and lat/lon, auto-setting the weather location |
| Lunar calendar | Gregorian/lunar comparison, solar terms, holiday display (incl. makeup workdays) |
| Stock quotes | Shanghai & Shenzhen stocks: overview + intraday / daily K / weekly K / monthly K / line chart + realtime board, multi-stock switching; 120s wake-refresh during market hours, sleeps until the next open otherwise (lunch break / after hours / weekends / holidays) |
| Image viewer | Monochrome BMP viewing; can be set as the standby wallpaper |
| WiFi provisioning | AP-mode hotspot + web configuration UI, provision by scanning a QR code; supports book/image/wallpaper upload and management |
| NTP time | Automatic network time sync; sync results written to the hardware RTC, which is preferred on wake-up |
| Hardware RTC | PCF8563 (onboard on v2, CR2032 backup, UTC timekeeping): system clock source + deep-sleep timed wake-up (second/minute countdowns and absolute-time alarms for long intervals, INT → GPIO4) |
| Battery management | ADC sampling of battery voltage, percentage display |
| Low power | Automatic deep sleep, button/timer wake-up, independent control of peripheral power |
| Error log | Custom panic handler; crash info written to Flash, viewable after reboot |

### 1.3 Hardware Platform

| Component | Model/Specification |
|-----------|---------------------|
| MCU | ESP32-C3 (RISC-V, 160MHz, 400KB SRAM) |
| Display | Waveshare 4.2" (400×300) / 2.7" (264×176) / 2.9" (296×128) e-paper (2.9" weather/calendar pages adapted to landscape; typography not finely tuned) |
| Storage | MicroSD card (SPI-connected, ≤32GB SD/SDHC, MBR partition table) |
| Flash | Built-in 4MB Flash |
| Input | 3 buttons: board_v1 legacy = GPIO9 direct key (key 1) + GPIO2 ADC ladder (keys 2/3); board_v2 new = all three keys on a GPIO4 ADC ladder |
| I2C devices | board_v2 only: PCF8563 hardware RTC (0x51) + AHT20 temperature/humidity (0x38), GPIO9=SDA / GPIO21=SCL |
| Power | Li-ion battery + ADC voltage sensing; v1 dual power rails (EINK=GPIO21 / SD=GPIO1, high = powered off), v2 single rail (GPIO1, low = powered on) |

### 1.4 Tech Stack

| Category | Technology | Purpose |
|----------|-----------|---------|
| Language | Rust (Edition 2024, nightly) | `#![no_std]` embedded environment |
| Target | `riscv32imc-unknown-none-elf` | ESP32-C3 RISC-V core |
| HAL | esp-hal v1.1.1 | Hardware abstraction layer |
| RTOS integration | esp-rtos v0.3.0 | Embassy async runtime integration |
| WiFi | esp-radio v0.18.0 | Wireless network driver |
| Network stack | embassy-net v0.9.0 | TCP/IP stack |
| Async runtime | embassy-executor v0.10.0 | Async task scheduling |
| Memory | esp-alloc v0.10.0 | Global heap allocator |
| Graphics | embedded-graphics v0.8 | Framebuffer drawing framework |
| Display driver | epd-waveshare (custom fork) | Low-level e-paper driver |
| Fonts | u8g2-fonts v0.7.1 + ab_glyph_rasterizer v0.1 | Bitmap Chinese (GB2312) + TTF vector rasterization |
| File system | embedded-sdmmc (fork `lfn-create` branch) | SD card FAT file system (supports creating Chinese file names) |
| Storage | esp-storage v0.9.0 | ESP32 internal Flash read/write |
| HTTP | reqwless v0.14.0 | HTTP client |
| TLS | embedded-tls v0.18 | Encrypted connections |
| JSON | mini-json (custom library) | Lightweight JSON parsing |
| Network addresses | no-std-net v0.6 | no-std network address types |

---

## 2. System Architecture

### 2.1 Overall Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Application (Pages)                       │
│  MainPage ─┬─ ReadPage (e-book)   ─┬─ WeatherPage (weather)      │
│            ├─ CalendarPage (calendar) ├─ ImagePage (images)     │
│            ├─ StockPage (stocks)      ├─ SettingPage (settings) │
│            └─ DebugPage (debug)                                  │
├─────────────────────────────────────────────────────────────────┤
│                      UI Widgets                                   │
│  IconGridWidget │ ListWidget │ Calendar │ TempChart │ KLine      │
│  WeatherIcon    │ QrcodeWidget│ Battery │ draw_icon │ ScrollBar  │
├─────────────────────────────────────────────────────────────────┤
│                      Services                                     │
│  Display (render) │ Event │ WiFi │ Storage (persist)             │
│  WorldTime (NTP)│ Weather│ Battery│ Sleep                       │
│  Request (HTTP) │ WebService│ SDMount │ TxtReader                │
│  Location (IP) │ TtfSd (vector fonts)                            │
├─────────────────────────────────────────────────────────────────┤
│              Board abstraction (v1 legacy / v2 new)               │
│  Power rails │ Key ladder/battery pins │ I2C (v2) │ PCF8563 │ AHT20 │
├─────────────────────────────────────────────────────────────────┤
│                      Data Models (Model)                          │
│  seniverse (weather) │ open_meteo (weather) │ lunar (terms)      │
│  holiday │ stock (K-line/board)                                   │
├─────────────────────────────────────────────────────────────────┤
│                    ESP Ecosystem (esp-rs v1.x)                    │
│  esp-hal │ esp-radio │ esp-storage │ esp-alloc │ esp-rtos        │
│  esp-bootloader-esp-idf │ esp-backtrace │ esp-println            │
├─────────────────────────────────────────────────────────────────┤
│                      Async Runtime (Embassy)                      │
│  embassy-executor v0.10 │ embassy-time v0.5 │ embassy-sync v0.8  │
│  embassy-net v0.9 │ embassy-futures                              │
├─────────────────────────────────────────────────────────────────┤
│                      Hardware                                     │
│  ESP32-C3 │ E-Paper Display │ SD Card │ Buttons │ Battery        │
│  PCF8563 RTC + AHT20 (I2C, v2 only)                              │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Module Dependency Graph

```
main.rs
 ├── board/     ◄── esp-hal (GPIO/I2C/RTCIO)        ← board abstraction (pins/power rails/I2C fixed per board at compile time)
 ├── display.rs ◄── epd-waveshare, embedded-graphics, critical-section
 ├── event.rs   ◄── embassy-sync, embassy-futures, esp-hal (GPIO/ADC), board
 ├── pages/     ◄── event, display, widgets, model, board
 │   ├── main_page.rs      (page manager/default home/back to last page on wake)
 │   ├── read/             ◄── epd2in9_txt, ttf_sd, sd_mount (layout chosen by feature)
 │   ├── weather/          ◄── weather, worldtime, location, aht20
 │   ├── calendar/         ◄── weather, worldtime, widgets::calendar, model::lunar
 │   ├── stock/            ◄── request, model::stock, widgets::kline, wifi
 │   ├── image_page.rs     ◄── sd_mount, flash_sleep
 │   ├── setting_page.rs   ◄── wifi, web_service, storage, location
 │   └── debug_page.rs     ◄── storage
 ├── wifi.rs    ◄── esp-radio, embassy-net, dhcparse
 ├── storage.rs ◄── esp-storage
 ├── weather.rs ◄── request, storage, worldtime, model::{seniverse,open_meteo,holiday}
 ├── location.rs ◄── request, wifi, mini-json
 ├── worldtime.rs ◄── sntpc, embassy-net, no-std-net, pcf8563
 ├── request.rs ◄── reqwless, embedded-tls
 ├── battery.rs ◄── esp-hal (ADC), board
 ├── sleep.rs   ◄── esp-hal (RTC), pcf8563, board, display
 ├── pcf8563.rs ◄── i2c_bus (compiled for v2; unused on v1)
 ├── aht20.rs   ◄── i2c_bus (compiled for v2)
 ├── i2c_bus.rs ◄── esp-hal (I2C)
 ├── ttf_sd.rs  ◄── ab_glyph_rasterizer, embedded-sdmmc
 ├── epd2in9_txt.rs ◄── embedded-sdmmc
 ├── sd_mount.rs ◄── embedded-sdmmc (fork lfn-create)
 ├── flash_sleep.rs ◄── esp-storage, embedded-graphics
 ├── web_service.rs ◄── wifi, embassy-net (TCP), sd_mount, flash_sleep
 ├── panic.rs   ◄── esp-backtrace, storage
 ├── model/     ◄── mini-json
 └── widgets/   ◄── embedded-graphics, u8g2-fonts
```

---

## 3. Build System & Configuration

### 3.1 Toolchain (`rust-toolchain.toml`)

```toml
[toolchain]
targets = ["riscv32imc-unknown-none-elf"]
# channel not pinned; default nightly
```

**Notes:**
- The ESP32-C3 uses the RISC-V 32-bit ISA (`riscv32imc`), no MMU, so standard OSes like Linux won't run. Rust can still target ESP-IDF's std mode (`riscv32imc-esp-espidf`) or esp-hal bare-metal; this project chooses the latter, hence `riscv32imc-unknown-none-elf`
- Nightly Rust is required for unstable features such as `generic_const_exprs` and `type_alias_impl_trait`
- esp-hal v1.x works well with recent nightlies; no need to pin a specific date

### 3.2 Cargo Configuration (`.cargo/config.toml`)

```toml
[target.riscv32imc-unknown-none-elf]
runner = "espflash flash --chip esp32c3 --baud 115200 --monitor --partition-table ./partitions.csv"

[build]
rustflags = [
  "-C", "link-arg=-Tlinkall.x",
  "-C", "force-frame-pointers",
]
target = "riscv32imc-unknown-none-elf"
```

**Differences from the old version:**
- Removed the `-Trom_functions.x` linker script (no longer needed with esp-hal v1.x)
- Kept the `linkall.x` memory-layout linker script
- `force-frame-pointers` kept for backtrace debugging

### 3.3 Flash Partition Table (`partitions.csv`)

```csv
# Name,   Type, SubType,  Offset,   Size, Flags
nvs,      data, nvs,      ,         0x6000,    ← non-volatile storage (24KB)
phy_init, data, phy,      ,         0x1000,    ← PHY init data (4KB)
factory,  app,  factory,  ,         3M,        ← application (3MB)
storage,  data, fat,      ,         128K,      ← FAT file system (128KB)
```

- The `nvs` partition (starting at `0x9000`) stores WiFi config, weather data, settings, etc.
- The `storage` partition (starting at `0x310000`) stores the standby wallpaper

### 3.4 Feature Flags (`Cargo.toml`)

Features are **two-dimensional**: `epd_*` selects the display driver, `board_*` selects the board hardware; a device = epd + board combination.

```toml
[features]
epd2in9 = []           # Display: 2.9" (296×128) — weather/calendar adapted to landscape, not finely tuned
epd2in7 = []           # Display: 2.7" (264×176)
epd4in2 = []           # Display: 4.2" (400×300)
board_v1 = []          # Board: v1 legacy (dual power rails, key 1 direct on GPIO9 + keys 2/3 on GPIO2 ADC ladder, no I2C)
board_v2 = []          # Board: v22 new (single power rail, three keys on GPIO4 ADC ladder, I2C with PCF8563 + AHT20, epd4in2 only)
enable_debug = []      # Enter debug page at boot if an error log exists in Flash
weather-openmeteo = [] # Switch weather source to Open-Meteo (Seniverse by default)
ttf_spike = []         # TTF benchmarking (dep:ab_glyph; does not affect the normal firmware)
```

`src/board/mod.rs` guards invalid combinations at compile time with `compile_error!`: exactly one display feature, exactly one board feature, and `board_v2` only pairs with `epd4in2`.

```bash
cargo run --release --features epd4in2,board_v2                # v22 new board, 4.2"
cargo run --release --features epd4in2,board_v1                # v1 legacy board, 4.2"
cargo run --release --features epd2in7,board_v1                # v1 legacy board, 2.7"
cargo run --release --features epd2in9,board_v1                # v1 legacy board, 2.9"
cargo run --release --features "epd4in2,board_v2,enable_debug"  # with debug mode
```

> The firmware exceeds the default debug size; `--release` is required to link into the 3MB factory partition. The `epd2in9` weather/calendar pages are adapted to 296×128 landscape; functional but not finely tuned.

### 3.5 Core Dependencies

```toml
# ── ESP ecosystem v1.x ──
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

# ── Graphics & display ──
embedded-graphics = "0.8"
epd-waveshare = { git = "https://github.com/longxiangam/epd-waveshare-modify" }   # custom fork
u8g2-fonts = { version = "0.7.1" }
ab_glyph_rasterizer = { version = "0.1.10", features = ["libm"] }  # TTF coverage rasterizer

# ── Storage (embedded-sdmmc uses the fork lfn-create branch: Chinese LFN creation) ──
embedded-sdmmc = { git = "https://github.com/longxiangam/embedded-sdmmc-rs", branch = "lfn-create" }

# ── Networking ──
reqwless = "0.14.0"
embedded-tls = "0.18"
no-std-net = "0.6"
sntpc = "0.3"
mini-json = { git = "https://github.com/longxiangam/mini-json" }   # custom JSON parser
```

**Main changes vs the old version:**

| Old (esp-hal 0.19) | New (esp-hal 1.x) | Notes |
|--------------------|--------------------|-------|
| `esp-hal` 0.19 | `esp-hal` 1.1.1 | Major HAL upgrade |
| `esp-wifi` 0.7 | `esp-radio` 0.18 | WiFi driver renamed/rewritten |
| `esp-hal-embassy` 0.2 | `esp-rtos` 0.3 | RTOS integration layer |
| `embedded-alloc` | `esp-alloc` 0.10 | ESP-specific allocator |
| *(none)* | `esp-bootloader-esp-idf` 0.5 | Bootloader support |
| `embassy-executor` 0.6 | `embassy-executor` 0.10 | Scheduler upgrade |
| `embassy-net` 0.4 | `embassy-net` 0.9 | Major network stack upgrade |
| `embassy-sync` 0.6 | `embassy-sync` 0.8 | Sync primitives upgrade |
| `embedded-hal-bus` 0.1 | `embedded-hal-bus` 0.3 | HAL bus upgrade |
| `reqwless` 0.11 | `reqwless` 0.14 | HTTP client upgrade |
| `embedded-sdmmc` 0.9 (crates.io) | fork `lfn-create` branch | Adds LFN (Chinese long file name) creation |
| *(none)* | `no-std-net` 0.6 | Network address types |
| *(none)* | `ab_glyph_rasterizer` 0.1 | TTF vector rasterization |
| `[patch.crates-io]` many patches | no patches (forks point to git repos) | Uses upstream crates / self-maintained forks |
| edition = "2021" | edition = "2024" | Rust edition upgrade |

---

## 4. Module Structure

```
src/
├── main.rs              # Entry point, hardware init, main loop (reading/normal restart split-mode)
├── panic.rs             # Custom panic handler (crashes written to Flash)
├── board/               # Board abstraction (epd_* picks display + board_* picks board; IO fixed at compile time)
│   ├── mod.rs           #   display/board compile_error! guards, shared EPD/SPI/SD pins, diagnostics
│   ├── v1.rs            #   v1 legacy: dual power rails, GPIO9 direct key + GPIO2 ADC ladder, no I2C
│   └── v2.rs            #   v22 new: single power rail, GPIO4 three-key ladder, I2C (PCF8563 + AHT20)
├── display.rs           # E-paper rendering service (dedicated Embassy task)
├── event.rs             # Event system (button detection + event queue + pub-sub dispatch)
├── pcf8563.rs           # PCF8563 hardware RTC (timekeeping/countdown/alarm, v2)
├── aht20.rs             # AHT20 temperature/humidity driver + sampling task (v2)
├── i2c_bus.rs           # I2C bus container (global Mutex + write/write_read helpers, v2)
├── sleep.rs             # Deep sleep & wake-up management (tiered PCF arming + reboot_sleep)
├── wifi.rs              # WiFi STA/AP modes (esp-radio)
├── worldtime.rs         # NTP time sync, global Clock (sync results written to PCF8563)
├── weather.rs           # Weather/holiday data fetching & caching (dual sources + 3-URL fallback)
├── location.rs          # IP-based auto location (ip-api)
├── request.rs           # HTTP/HTTPS GET client
├── battery.rs           # Battery voltage ADC sampling
├── storage.rs           # Flash persistent storage (NVS area)
├── sd_mount.rs          # SD card mounting & file ops (fork LFN: Chinese file name creation)
├── ttf_sd.rs            # On-demand TTF glyph parsing & rasterization from SD (reading mode)
├── epd2in9_txt.rs       # TXT text pagination engine (multiple display sizes)
├── flash_sleep.rs       # Standby wallpaper Flash storage & rendering
├── web_service.rs       # Provisioning web service (book/image/wallpaper upload & management + JSON API)
├── random.rs            # Hardware RNG → rand_core wrapper
├── utils.rs             # make_static! macro
│
├── model/               # Data model layer
│   ├── mod.rs
│   ├── seniverse.rs     # Seniverse weather API data structures
│   ├── open_meteo.rs    # Open-Meteo API data conversion
│   ├── holiday.rs       # Holiday data structures
│   ├── lunar.rs         # Lunar calendar/solar terms/zodiac algorithms
│   └── stock.rs         # Stock model (KLine/ChartMode/RealtimeQuote + parsing + intraday cache)
│
├── widgets/             # UI widget layer
│   ├── mod.rs
│   ├── icon_grid_widget.rs  # Icon grid menu
│   ├── list_widget.rs       # Scrollable list
│   ├── weather_icon.rs      # Weather icons (BMP)
│   ├── temp_chart.rs        # Temperature trend chart
│   ├── calendar.rs          # Calendar grid (Gregorian + lunar + terms + holidays)
│   ├── kline.rs             # Stock candlestick/line chart
│   ├── qrcode_widget.rs     # QR code widget
│   ├── battery.rs           # Battery icon
│   ├── draw_icon.rs         # Status icons (wifi/loading/moon)
│   └── scroll_bar.rs        # Scroll bar
│
└── pages/               # Page layer (each subdirectory has mod.rs/page.rs/layout*.rs)
    ├── mod.rs               # Page trait, main_task / reading_task, PageEnum
    ├── main_page.rs         # Main menu page (page manager/default home/back to last page on wake)
    ├── debug_page.rs        # Error log debug page
    ├── image_page.rs        # Image viewer page
    ├── setting_page.rs      # Settings/provisioning page
    ├── read_menu_page.rs    # Placeholder (todo!(), unused)
    ├── read/                # E-book reading (TTF dynamic pagination)
    │   ├── page.rs          #   ReadPage state machine (book list/reading/menu/bookmarks/typography/fonts)
    │   ├── bitmap.rs        #   Bitmap font drawing
    │   └── layout_*.rs      #   Size chosen by feature (400x300 / 176x264 / 128x296)
    ├── weather/             # Weather page (layout_264x176 / layout_400x300 / layout_128x296 + render_data)
    ├── calendar/            # Lunar calendar page (layout_* + render_data)
    └── stock/               # Stock page (single layout file + render_data)
```

---

## 5. Core Modules

### 5.1 main.rs — Entry Point & Hardware Init

**File:** `src/main.rs`

#### 5.1.1 Program Entry

```rust
#![no_std]   // no standard library
#![no_main]  // no standard main entry

esp_bootloader_esp_idf::esp_app_desc!();  // generates the app description

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // ...
}
```

**Key changes (vs the old version):**
- Entry macro changed from `#[esp_hal_embassy::main]` to `#[esp_rtos::main]`
- Return type is now explicit `-> !` (never returns)
- `esp_bootloader_esp_idf::esp_app_desc!()` generates an IDF-compatible app description

#### 5.1.2 Initialization Flow

```
DRAM2 heap region registration (~64KB .dram2_uninit section → esp_alloc::HEAP.add_region)
  │
hal_init(HalConfig::default().with_cpu_clock(CpuClock::max()))  ← HAL init
  │
board::init_pad_hold()            ← v2: release the hold armed last deep sleep / v1: hold GPIO1
  │
board::init_i2c()                 ← v2: build the I2C bus (PCF8563+AHT20); v1: None
  │
board::sd_miso_pullup()           ← GPIO10 internal pull-up (SD compatibility, before SPI claims the pin)
  │
board::init_power_rails()         ← power rails (v2 single GPIO1 / v1 dual GPIO21+GPIO1)
  │
TimerGroup + SoftwareInterruptControl     ← Embassy timers
  │
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0)  ← start RTOS
  │
Rtc::new(peripherals.LPWR)                ← init RTC
  │
storage::enter_process()                  ← load config from Flash
  │
display::reload_full_refresh_times()      ← load full-refresh interval cache
  │
reset_reason / wakeup_cause printout      ← reset & wake reason
  │
pcf8563::consume_timer_wake()             ← read PCF TF/AF to classify timed wake, clear flags
  │                                         (or the ESP RTC Timer fallback wake)
sleep::set_timed_wake(timed, window)      ← 8s window if weather data is stale, else none
  │
Not a timed wake → display::mark_need_clear()   ← force full refresh on the first frame
  │
ADC config (key ladder / battery — pins per board via board::*)
  │
Button digital input + pull-up (configured again after ADC init)
  │
board::spawn_sensors()            ← v2: start the AHT20 sampling task
  │
spawner.spawn(event::run)         ← button detection task (v2 single ladder / v1 dual path)
spawner.spawn(event::dispatch)    ← event dispatch task
  │
GPIO2 deep-sleep isolation (slp_sel=1 + IE/WPU/WPD=0; battery pin high-Z in sleep, no leakage)
  │
SPI init (SpiConfig + Rate::from_mhz(32))  ← create SPI bus
  │
SPI bus sharing (CsMutex + CriticalSectionDevice)  ← EPD/SD share
  │
spawner.spawn(display::render)            ← start render task
  │
SdCard + VolumeManager                    ← mount SD card
  │
add_rtcio(key ladder GPIO, Low)           ← RTC IO wake source (reading-mode sleep relies on it too)
  │
Reading-mode decision (rtc_fast READING_MODE or NVS "was reading")
  │
  ├── Reading mode → worldtime::init_clock_pcf() (restore clock from PCF)
  │              + spawn(battery) + spawn(pages::reading_task); main idles
  └── Normal mode
       ├── WiFi not configured → AP provisioning mode (SettingPage loop)
       └── Configured → STA mode
            ├── spawn(battery::test_bat_adc)
            ├── spawn(worldtime::ntp_worker)
            ├── spawn(pages::main_task)
            └── connect_wifi()
```

#### 5.1.3 Heap Allocator (esp-alloc)

The heap buffer lives in a dedicated **dram2 section** (~64KB of DRAM released by the bootloader, previously idle):

```rust
#[unsafe(link_section = ".dram2_uninit")]
static mut DRAM2_HEAP: core::mem::MaybeUninit<[u8; 65280]> = core::mem::MaybeUninit::uninit();

// registered as a heap region in main
esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(heap_ptr, 65280, ...));
```

**Rationale:**
- All of main DRAM is left for the stack (the TTF rendering async call chain is deep; a 64KB heap in main DRAM once caused a stack overflow)
- The heap is still large enough (WiFi ~60KB / TtfWs ~44KB), and the stack-overflow watchdog (watchpoint at `_stack_end`) is unaffected
- Reading and normal modes share the same heap; no per-mode split needed

#### 5.1.4 HAL Init

```rust
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let peripherals = hal_init(config);
```

In esp-hal v1.x, `hal_init(HalConfig)` does all initialization in one call and returns `Peripherals`; no manual system clock controller management.

#### 5.1.5 New GPIO API

```rust
// New: GPIOs come directly from peripherals
let epd_busy = peripherals.GPIO6;
let epd_cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
```

GPIOs are taken directly from `Peripherals` — no intermediate `Io` object. `Output::new` takes an `OutputConfig` parameter. Board-related button/battery/I2C pins are always obtained via `crate::board::*` (see 5.12).

#### 5.1.6 New SPI API

```rust
let spi = Spi::new(
    peripherals.SPI2,
    SpiConfig::default()
        .with_frequency(Rate::from_mhz(32))
        .with_mode(Mode::_0),
).unwrap()
.with_sck(epd_sclk)
.with_miso(epd_miso)
.with_mosi(epd_mosi);
```

`SpiConfig` struct + `Rate::from_mhz(32)`; both boards share the same SPI bus and pins.

#### 5.1.7 SPI Bus Sharing

```rust
use critical_section::Mutex as CsMutex;

let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);

let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();
```

`critical_section::Mutex` + `CriticalSectionDevice::new_no_delay()` (embedded-hal-bus 0.3).

#### 5.1.8 Embassy RTOS Startup

```rust
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

#### 5.1.9 Wake Reason Classification & Timed Wake

At boot the wake source is classified, driving the "sleep right after refreshing" policy:

```rust
// PCF TF=1 → PCF timed wake (PCF INT pulls the key-ladder GPIO low); ESP RTC Timer → fallback wake
let timed_wake = crate::pcf8563::consume_timer_wake().await
    || matches!(wake_reason, esp_hal::system::SleepSource::Timer);
// A network refresh is only needed when weather data is stale (8s window for WiFi+sync);
// fresh data means no window: render and go back to sleep immediately
let stale = crate::weather::Weather::weather_stale().await;
crate::sleep::set_timed_wake(timed_wake, if stale { 8_000 } else { 0 });
// Non-timed wake: force a full refresh on the first frame to clear ghosting
if !timed_wake {
    crate::display::mark_need_clear();
}
```

#### 5.1.10 Restart Split-Mode (Reading / Normal)

Reading mode is a **restart split-mode**: at startup it skips WiFi init and owns the whole heap for TTF (large glyph cache + deep stack).

- The mode flag `READING_MODE` lives in rtc_fast (preserved across reboot_sleep deep sleep)
- On the v2 board, IO (RTCIO) wake-ups lose rtc_fast → restored from NVS `SleepPageStorage` (sentinel `page == 100` for "was reading")
- Reading mode has no ntp_worker; `worldtime::init_clock_pcf()` restores the clock from the PCF8563
- Exiting reading → `reading_task` clears the flag → `reboot_sleep()` (50ms short deep sleep) back to normal mode

### 5.2 display.rs — Rendering Service

**File:** `src/display.rs`

#### 5.2.1 Architecture

The display system uses a **producer-consumer** pattern: pages draw into the framebuffer and send render requests through a Channel.

```
┌──────────────┐    RENDER_CHANNEL    ┌──────────────┐    SPI    ┌─────────┐
│ Pages        │ ──── RenderInfo ───► │ render task  │ ────────► │ EPD     │
└──────────────┘                       └──────────────┘           └─────────┘
```

#### 5.2.2 Key Types

```rust
// Conditional compilation picks the display size
#[cfg(feature = "epd2in9")]
pub type EpdDisplay = Display2in9;          // 296×128
#[cfg(feature = "epd2in7")]
pub type EpdDisplay = Display2in7;          // 264×176
#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;          // 400×300

pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd4in2<SPI, BUSY, DC, RST, DELAY>; // likewise per feature

// SPI device type
type ActualSpi<'a> = CriticalSectionDevice<
    'a,
    Spi<'a, esp_hal::Blocking>,
    Output<'a>,
    embedded_hal_bus::spi::NoDelay,
>;
```

#### 5.2.3 RTC Memory Attribute Change

```rust
// New: unstable wrapper
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES: u32 = 0;

// Old: plain rtc_fast
// #[ram(rtc_fast)]
```

In esp-hal v1.x the `#[ram(...)]` attribute requires the `unstable()` wrapper.

#### 5.2.4 Unsafe Access Pattern Change

```rust
// New: explicit core::ptr operations
unsafe { core::ptr::addr_of_mut!(DISPLAY).write(Some(display)); }
unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut().unwrap().buffer() }
```

esp-hal v1.x is stricter about `static mut` access; `core::ptr::addr_of_mut!` avoids undefined behavior.

#### 5.2.5 GPIO Task Parameters

```rust
#[embassy_executor::task]
pub async fn render(
    mut spi_device: &'static mut ActualSpi<'static>,
    busy: esp_hal::peripherals::GPIO6<'static>,   // with lifetime
    rst: esp_hal::peripherals::GPIO7<'static>,
    dc: esp_hal::peripherals::GPIO20<'static>,
)
```

GPIOs are generic `esp_hal::peripherals::GPIOx<'static>` parameters. Both boards share the EPD control pins (busy=6 / rst=7 / dc=20).

#### 5.2.6 Refresh Strategy

E-paper supports partial (Quick) and full (Full) refresh. Every N quick refreshes forces a full refresh to clear ghosting: the platform default N is a constant (`epd2in7` = 20, others = 5), overridable at runtime from the settings page / web config (`DisplayStorage.full_refresh_times`, clamped to `1..=100` by `reload_full_refresh_times()`):

```rust
// non-epd2in7 path
let need_clear = take_need_clear();   // true on the first frame after a cold boot (rtc_fast)
let need_force_full = (get_render_times() % get_full_refresh_times() == 0 || need_clear)
    && refresh_lut == RefreshLut::Quick;
if need_force_full {
    spi_device = set_refresh_mode(RefreshLut::Full, &mut epd, spi_device);
}
```

The 2.7" (`epd2in7`) path differs: `PREV_BUFFER` (5808B) keeps the previous frame for differential updates; wake/first frame uses `set_base_map_quiet` for a silent transition; regular frames go through `update_and_display_frame_partial`; periodic `set_base_map` full refreshes. `RENDER_TIMES`, `NEED_CLEAR`, and `PREV_BUFFER` all live in rtc_fast and survive deep sleep.

#### 5.2.7 Render/Sleep Coordination

- `await_render_idle()`: before sleeping or cutting power, waits until `RENDER_BUSY=false` and `RENDER_CHANNEL` is drained — cutting power mid-refresh produces gray ghosting that only a full refresh clears
- `set_sleep_renderer()`: pages register their own "sleep screen" renderer (e.g. the stock page renders current quotes as the standby image), invoked by `show_sleep()`; reset to None when the page's run loop exits

### 5.3 event.rs — Event System

**File:** `src/event.rs`

#### 5.3.1 Event Types

```rust
pub enum EventType {
    KeyShort(u32),       // short press
    KeyLongStart(u32),   // long press start
    KeyLongIng(u32),     // long press ongoing (throttled to one per 100ms to avoid ~1kHz busy spin)
    KeyLongEnd(u32),     // long press release
    KeyDouble(u32),      // double click
    WheelBack,           // wheel back (reserved)
    WheelFront,          // wheel forward (reserved)
}
```

#### 5.3.2 Pub-Sub Mechanism

```rust
struct Listener {
    callback: Box<dyn FnMut(EventInfo) -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + Sync>,
    event_type: EventType,
    ptr: Option<usize>,
    fixed: bool,
}

static LISTENER: Mutex<CriticalSectionRawMutex, Vec<Listener, 20>> = Mutex::new(Vec::new());
```

Registration:
- `on(event_type, callback)` — one-shot listener
- `on_target(event_type, ptr, callback)` — bound to a specific object
- `on_fixed(event_type, ptr, callback)` — persistent listener
- `clear()` — remove non-fixed listeners

#### 5.3.3 Event Queue & Dispatch (producer-consumer)

Button scanning and callback execution are decoupled: scanning only enqueues events, and a dedicated `dispatch` task drains the FIFO serially — **callbacks running long no longer lose button presses**.

```rust
/// Event queue: capacity 8. Producer = toggle_event (scan/state machine), consumer = dispatch task.
static EVENT_QUEUE: Mutex<CriticalSectionRawMutex, Vec<EventType, 8>> = Mutex::new(Vec::new());
static EVENT_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Enqueue (producer). Consecutive KeyLongIng events merge at the tail; when full, drop oldest.
pub async fn toggle_event(event_type: EventType, _ms: u64) {
    let mut q = EVENT_QUEUE.lock().await;
    if let EventType::KeyLongIng(k) = event_type {
        if let Some(EventType::KeyLongIng(k2)) = q.last() {
            if *k2 == k { q.pop(); }   // same-key repeat → replace in place, no pile-up
        }
    }
    if q.len() >= q.capacity() { q.remove(0); }  // full: drop oldest, keep newest
    q.push(event_type).ok();
    drop(q);
    EVENT_READY.signal(());
}

/// Dispatch task: FIFO serial dispatch to listener callbacks.
#[embassy_executor::task]
pub async fn dispatch() {
    loop {
        EVENT_READY.wait().await;
        loop {
            // pop one event → dispatch_event (holds the LISTENER lock, callbacks run serially)
        }
    }
}
```

Constraint: dispatching holds the `LISTENER` lock and runs callbacks serially — callbacks must **not** call `on/clear/on_fixed` to rebind events (deadlock); rebinding still follows the page run-loop convention.

#### 5.3.4 Button Detection (two per-board entry points)

```rust
// v2 new board: all three keys share GPIO4 (digital input + ADC ladder + deep-sleep wake on one pad)
#[embassy_executor::task]
#[cfg(feature = "board_v2")]
pub async fn run(key2: crate::board::KeyLadderGpio) {
    let mut key_all = Input::new(key2, InputConfig::default().with_pull(Pull::Up));
    loop {
        key_all.wait_for_falling_edge().await;   // all presses pull the pin digitally low
        let num = judge_adc_num().await;         // sample immediately after the edge (median debounces)
        if num == 0 { Timer::after_millis(10).await; continue; }
        key_detection(&mut key_all, num).await;
        refresh_active_time().await;
        crate::sleep::mark_user_touch();
    }
}

// v1 legacy board: digital key GPIO9 (key 1) + key ladder GPIO2 (keys 2/3 via ADC); two falling-edge select
#[embassy_executor::task]
#[cfg(feature = "board_v1")]
pub async fn run(key1: esp_hal::peripherals::GPIO9<'static>, key2: crate::board::KeyLadderGpio) { /* ... */ }
```

Key-number thresholds differ per board (`judge_adc_num`):
- **v2** (GPIO4 ladder, keys 1/2/3 all on the ladder, measured ≈44/409/645): <220→key 1, 220~520→key 2, 520~900→key 3
- **v1** (GPIO2 ladder, only keys 2/3 on the ladder): <200→key 2, 200~1000→key 3, >1000 glitch ignored

Every physical button press also calls `sleep::mark_user_touch()` (exits "sleep right after refresh" mode, see 5.6).

#### 5.3.5 ADC Type Changes

```rust
// ADC pin type follows the board (board::KeyLadderGpio: v2=GPIO4 / v1=GPIO2)
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<
    AdcPin<crate::board::KeyLadderGpio, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>
>> = Mutex::new(None);

pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<
    Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>
>> = Mutex::new(None);
```

### 5.4 wifi.rs — WiFi & Network Services

**File:** `src/wifi.rs`

**This module changed the most** in the upgrade: fully migrated from `esp-wifi` to `esp-radio`.

#### 5.4.1 New WiFi Init API

```rust
use esp_radio::wifi::{
    Config as WifiConfig,
    ControllerConfig,
    Interface,
    WifiController,
    sta::StationConfig,
    ap::AccessPointConfig,
};

pub async fn connect_wifi(spawner: &Spawner, rng: Rng, wifi: esp_hal::peripherals::WIFI<'static>) {
    let station_config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(ssid.as_str())
            .with_password(password.as_str().into()),
    );

    let (controller, interfaces) = esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    ).unwrap();

    let wifi_interface = interfaces.station;
    // ...
}
```

**Differences:**

| Old (esp-wifi 0.7) | New (esp-radio 0.18) |
|--------------------|----------------------|
| `esp_wifi::initialize(EspWifiInitFor::Wifi, timer, rng, radio_clk, &clocks)` | not needed |
| `esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)` | `esp_radio::wifi::new(wifi, config)` |
| returns `(interface, controller)` | returns `(controller, interfaces)` |
| `Rng::new(peripherals.RNG)` | `Rng::new()` no args |
| needs `TIMG0`, `RADIO_CLK`, `&clocks` | only the `wifi` peripheral |

#### 5.4.2 New Network Stack API

```rust
// New: embassy_net::new() returns (Stack, Runner)
let (stack, runner) = embassy_net::new(
    wifi_interface,
    config,
    make_static!(StackResources::<4>::new()),
    seed,
);
let stack: &Stack<'static> = &*make_static!(stack);

spawner.spawn(net_task(runner).unwrap());  // Runner is a separate task
```

#### 5.4.3 New WiFi Event Listening API

```rust
// New: subscribe + next_event_pure
loop {
    let mut subscriber = controller.subscribe().unwrap();
    let close_signal = STOP_WIFI_SIGNAL.wait();
    match select(subscriber.next_event_pure(), close_signal).await {
        Either::First(_) => {
            drop(subscriber);
            if !controller.is_connected() { /* disconnected */ }
        }
        Either::Second(_) => {
            drop(subscriber);
            let _ = controller.disconnect_async().await;
        }
    }
}
```

#### 5.4.4 WiFi Lifecycle

```
                    ┌──────────────────┐
                    │  WifiStopped     │
                    └────────┬─────────┘
                             │ RECONNECT_WIFI_SIGNAL
                             ▼
  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
  │ WifiDisconnected│──►│WifiConnecting │──►│WifiConnected │
  └──────────────┘   └──────────────┘   └──────┬───────┘
         ▲                                       │
         │  is_connected() == false              │ STOP_WIFI_SIGNAL
         └───────────────────────────────────────┘
```

#### 5.4.5 AP Mode

```rust
let ap_config = WifiConfig::AccessPoint(
    AccessPointConfig::default()
        .with_ssid("esp_wifi")
        .with_password("123456789".into())
);

let (controller, interfaces) = esp_radio::wifi::new(wifi, ControllerConfig::default().with_initial_config(ap_config)).unwrap();
let wifi_ap_interface = interfaces.access_point;
```

In AP mode the ESP32 runs its own DHCP + DNS services:
- **DHCP**: listens on UDP 67, assigns IP `192.168.2.2`
- **DNS**: listens on UDP 53, hijacks all domains to `192.168.2.1`

#### 5.4.6 UDP API Change

```rust
// New recv_from returns (usize, UdpMetadata)
match udp_socket.recv_from(&mut buf).await {
    Ok((n, src)) => { /* src is UdpMetadata */ }
}
```

### 5.5 storage.rs — Persistence

**File:** `src/storage.rs`

#### 5.5.1 Flash Access

```rust
pub fn write_flash(flash_addr: u32, bytes: &[u8]) -> Result<(), FlashStorageError> {
    let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
    let mut flash = FlashStorage::new(flash);
    flash.write(flash_addr, bytes)
}
```

#### 5.5.2 Storage Layout

```
NVS partition (starts 0x9000)
  ├── VersionStorage        offset +0x00     version + init tag (0x1234_abce)
  ├── WifiStorage           offset +sizeof    WiFi SSID + password + configured flag
  ├── WeatherStorage        offset +sizeof    weather token + city + sync time + cached data
  ├── SleepStorage          offset +sizeof    reading sleep / weather(calendar) sleep durations
  ├── OtherStorage          offset +sizeof    reused: default home (0=books/1=weather/2=calendar/3=stocks/9=menu)
  ├── HolidayStorage        offset +sizeof    holiday token + sync time + cached data
  ├── ErrorLogStorage       offset +sizeof    error count + last error message
  ├── StockStorage          offset +sizeof    up to 6 (code,name) pairs + count + selected
  ├── DisplayStorage        offset +sizeof    full-refresh interval (0 = unconfigured, clamped display-side)
  ├── ReadingSettings       offset +sizeof    reading typography: font size + line gap (i8) + rotation + font file name (32B)
  ├── SyncStampStorage      offset +sizeof    "SYND": manual time/holiday sync timestamps + show-time switch
  ├── SleepPageStorage      offset +sizeof    "PAGE": last page before sleep (100 = reading-mode sentinel)
  ├── ReadingFlagsStorage   offset +sizeof    "PGSW": reading key 1/2 swap
  └── StrokeConfigStorage   offset +sizeof    "STKE": reading stroke bolding factor ×10 (10..=30)

Storage partition (starts 0x310000)
  └── standby wallpaper     8-byte header + pixel data
```

- Everything from `StockStorage` on was **appended at the end without moving existing offsets**, so `INIT_TAG` never changes — raw bytes left by older firmware are clamped by consumers or validated with magic fields (`SYND`/`PAGE`/`PGSW`/`STKE`)
- `SyncStampStorage` reuses the old ADC calibration slot (that mechanism was removed with the fixed-threshold button design)
- `SleepPageStorage`: written on page change — IO (RTCIO) wake-ups make rtc_fast unreliable, so this restores "last page" and "was reading"

#### 5.5.3 Serialization & Macros

Zero-copy serialization via `unsafe` pointer casts; the `impl_storage!` macro implements read/write for each struct. `HolidayStorage` uses chunked writes (128 bytes per chunk) to avoid stack overflow. High-frequency writers like `write_sleep_page` skip the Flash write when the value is unchanged, avoiding wear.

### 5.6 sleep.rs — Low-Power Management

**File:** `src/sleep.rs`

#### 5.6.1 Deep Sleep Flow

```
to_sleep_tips(sleep_time, idle_time, show_sleep)
  │
  timed_hold_off()? ── yes → return (no sleep during a timed-wake data-refresh window)
  │
  immediate check: TIMED_WAKE && !USER_TOUCHED && sleep_time > 0
  │     (timed wake + user hasn't touched buttons + page finished rendering → sleep now)
  │
  Not immediate: idle time not reached → return
  │
  force_stop_wifi()
  │
  Tiered timed-wake arming (sleep_time > 0, see 5.6.3)
  │
  show_sleep → show_sleep() (page-registered sleep_renderer)
  │
  await_render_idle() — wait for the render cycle to finish and drain (mid-refresh power cut = gray ghosting)
  │
  WHEN_SLEEP_RTC_MS = get_rtc_ms()   ← rtc_fast, for get_sleep_ms estimates
  │
  Power rails off (EINK_PWER_PIN / SD_PWER_PIN taken and set high; empty slots skipped)
  │
  save_time_to_rtc()
  │
  Rtc.sleep_deep()
```

Time estimation after wake: `get_sleep_ms() = current RTC ms - WHEN_SLEEP_RTC_MS`, combined with `WHEN_SLEEP_TIME_TIMESTAMP` (rtc_fast) to infer the current time.

#### 5.6.2 Timed-Wake State & Refresh Window

```rust
static mut TIMED_WAKE: bool = false;      // was this wake a timed wake?
static mut TIMED_HOLD_OFF_UNTIL: u64 = 0; // data-refresh window deadline (ms)
static mut USER_TOUCHED: bool = false;    // has the user pressed a button since the timed wake?
```

- `set_timed_wake(timed, hold_off_ms)`: set at boot in main. An 8s window when weather data is stale (waiting for WiFi/sync/AHT20 first sample), none when fresh; also clears `USER_TOUCHED`
- `timed_hold_off()`: while the window is open, `to_sleep_tips` refuses to sleep
- `idle_for(default)`: returns 1 hour during the window (never idle), otherwise the page's configured value
- `mark_user_touch()`: called by `event::run` on every physical press — clears "sleep right after refresh", falling back to the normal idle flow

#### 5.6.3 Tiered Timed-Wake Arming (PCF8563 first, ESP RTC fallback)

```
sleep_time = 0            → no arming (button-wake sleep)
≤ 255 s                   → PCF countdown 1Hz seconds tier (arm_timer_secs)
≤ 255 min                 → PCF countdown 1/60Hz minutes tier (arm_timer_minutes, rounded up, at most 59s extra)
> 255 min (with PCF)       → PCF absolute-time alarm (arm_alarm_at, set in UTC per the PCF's own timekeeping)
                            (the coin-cell 32k crystal keeps accurate time; the MCU's deep-sleep slow clock drifts)
PCF arming failed / no PCF (v1) → TimerWakeupSource (ESP RTC timer) fallback
```

Wake sources: key-ladder GPIO (RtcioWakeupSource, Low level) + the timer source. The PCF INT line shares the key-ladder pin (v2 = GPIO4); pulling it low when the timer fires wakes the CPU.

#### 5.6.4 reboot_sleep — Mode Switching

```rust
pub async fn reboot_sleep() { /* short deep sleep with a 50ms timer wake */ }
```

"Reboot" mode switching: a deep-sleep wake re-runs main (re-deciding WiFi, re-allocating the heap) while **preserving rtc_fast** (mode flags/book index/resume offset), so switching modes needs neither software_reset (which clears rtc_fast) nor flash. EINK/SD power is cut as in a normal deep sleep; main powers them back up after the wake.

### 5.7 worldtime.rs — NTP Time Sync

**File:** `src/worldtime.rs`

#### 5.7.1 Network Address Types

```rust
use no_std_net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
```

#### 5.7.2 NTP Socket Async Interface

```rust
impl<'a> NtpUdpSocket for NtpSocket<'a> {
    fn send_to<T: ToSocketAddrs + Send>(
        &self, buf: &[u8], addr: T,
    ) -> impl core::future::Future<Output = sntpc::Result<usize>> {
        async move { /* ... */ }
    }

    fn recv_from(&self, buf: &mut [u8])
        -> impl core::future::Future<Output = sntpc::Result<(usize, SocketAddr)>>
    {
        async move { /* ... */ }
    }
}
```

#### 5.7.3 Timestamp Safety

```rust
// validity check added
if ts > 1577836800 && ts < 4102444800 {  // year 2020–2100
    if let Ok(now) = OffsetDateTime::from_unix_timestamp(current_second as i64) {
        clock.set_time(now).await;
    }
}
```

#### 5.7.4 PCF8563 Clock Source (restore & write)

- **Restore**: `ntp_worker` first calls `pcf_restore()` — reads UTC time from the PCF8563 to restore the Clock (backup battery maintained, authoritative); on failure it falls back to the rtc_fast estimate (`WHEN_SLEEP_TIME_TIMESTAMP + get_sleep_ms()`, sanity-checked)
- **Write**: after a successful NTP sync, `pcf8563::write_time(ts)` writes UTC time into the PCF8563 (STOP=1 pauses timekeeping to prevent rollback tearing, cleared afterwards) and records the `SyncStampStorage` time stamp
- **Reading mode**: no ntp_worker; main calls `init_clock_pcf()` to restore the clock from the PCF — time display/cross-day decisions no longer lag for lack of WiFi (stock cross-day detection once broke because of this)

Other notes:
- NTP server pool `["ntp.aliyun.com", "ntp.tuna.tsinghua.edu.cn", "ntp1.aliyun.com"]` is iterated; **a DNS failure moves to the next server**, and only exhausting the list returns an error (no single point of failure).
- `TimestampGen::timestamp_sec()` must return UNIX seconds (sntpc adds the NTP epoch offset internally); microseconds distort roundtrip/offset.
- `ntp_worker` syncs hourly (3600s) by default with backoff on failure (1s/10s, longer intervals after >5 consecutive failures), and chains weather & holiday syncs after a successful time sync.
- `CLOCK_RESTORED_THIS_BOOT` (normal RAM, reset each boot) vs `CLOCK_SYNC_TIME_SECOND` (rtc_fast): UI time display should be based on the former, to avoid showing 1970 when the Clock is still `UNIX_EPOCH` right after a wake.

### 5.8 request.rs — HTTP Client

**File:** `src/request.rs`

```rust
pub struct RequestClient {
    stack: &'static Stack<'static>,  // simplified Stack type
    rng: RngWrapper,
    // ...
}
```

- `Stack<WifiDevice<'static, WifiStaDevice>>` simplified to `Stack<'static>`
- TLS uses `UnsecureProvider::new::<Aes128GcmSha256>(&mut self.rng)`
- `reqwless` v0.14 changed the Response API

### 5.9 battery.rs — Battery Management

**File:** `src/battery.rs`

```rust
#[ram(unstable(rtc_fast))]
static mut LAST_BATTERY_PERCENT: u32 = 0;
```

Samples every 60 seconds (`voltage = adc × 2`, divider factor; percentage via a squared curve). The ESP32-C3 `read_oneshot` often returns `Err` on cold boot / channel switch, so the strategy is **retry on error, give up beyond the limit**: at most 50 retries keeping the previous percentage; only exceeding the limit skips the round, avoiding an infinite loop while holding the lock. `LAST_BATTERY_PERCENT` mirrors into rtc_fast; `last_percent()` lets the `sleep_renderer` read it synchronously in the render path (no Mutex await). The battery divider pin follows `board::BatAdcGpio` (v2=GPIO2 / v1=GPIO4).

### 5.10 panic.rs — Custom Panic Handler

**File:** `src/panic.rs`

```rust
use esp_backtrace::Backtrace;

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    // ...
    let backtrace = Backtrace::capture();
    for frame in backtrace.frames() {
        let _ = write!(error_msg, "  #{} 0x{:x}\n", stack_count, frame.program_counter());
    }
    // ...
}
```

### 5.11 Remaining Modules

| Module | Description |
|--------|-------------|
| `location.rs` | IP-based auto location: queries `ip-api.com` (plain HTTP) for city and lat/lon, producing `"lat:lon"` for Seniverse queries. Uses the standard `use_wifi` three-step pattern |
| `weather.rs` | Dual data sources (`weather-openmeteo` feature); unified `request()` entry; holiday source with 3-URL fallback (`cdn.jsdelivr.net` → `fastly.jsdelivr.net` → `raw.githubusercontent.com`; the raw origin is SNI-blocked inside China yet reachable abroad), up to 3 retries with a 60s cooldown; rtc_fast cross-sleep flags; `weather_stale()` / `stale_in_secs()` (5h sync interval) drive sleep scheduling |
| `epd2in9_txt.rs` | TXT pagination engine; per-feature `LINES_NUM/WIDTH/HEIGHT` for 2.9/2.7/4.2; `.log` file semantics: `[0]` = last reading byte offset (bit31 = finished), `[1..]` = bookmark byte offsets |
| `sd_mount.rs` | embedded-sdmmc wrapper; LFN (Chinese long file names) provided by the **fork `lfn-create` branch** (the old hand-written LFN directory-entry code was deleted); book names up to 150 bytes (~40 Chinese chars) |
| `ttf_sd.rs` | On-demand random glyph reads from SD (no whole-file loading): self-implemented TTF parsing (table directory→cmap→loca→hmtx→glyf) with per-glyph seek+read; quadratic curves rasterized by ab_glyph_rasterizer (adaptive flattening + coverage threshold); full cmap subtable cache (28KB) + loca/glyf SD fallback; the `TtfWs` workspace is heap-allocated (created on entering reading, freed on exit, mutually exclusive with WiFi heap use at runtime); stroke bolding factor (fixes thin-stroke gaps) |
| `flash_sleep.rs` | Standby wallpaper Flash storage (`"SLEP"` magic + crash-safe writes); BMP→1bit conversion (ITU-R BT.601 grayscale); 300×400 resolution |
| `web_service.rs` | Provisioning web service (see 5.4.5); routes cover listing/uploading/deleting books, images and wallpapers, WiFi/weather/sleep/display/stock/reading config forms, and a `/config` JSON API; reads the body across TCP segments by Content-Length (large uploads don't drop fields) |
| `random.rs` | `esp_hal::rng::Rng` → `rand_core::RngCore + CryptoRng` wrapper for TLS/QR |

### 5.12 board/ — Board Abstraction

**Files:** `src/board/{mod.rs, v1.rs, v2.rs}`

The two-dimensional features fix the entire device's IO assignment at compile time: `epd_*` picks the display driver, `board_*` picks the board. `mod.rs` guards invalid combinations with `compile_error!` (one display feature, one board feature, v2 pairs only with epd4in2) and re-exports via `pub use v1::*` / `pub use v2::*` — callers always write `board::xxx`, never the concrete board file.

**Build matrix:**

| Combination | Command |
|-------------|---------|
| v2 new board 4.2 | `--features epd4in2,board_v2` |
| v1 legacy 4.2 | `--features epd4in2,board_v1` |
| v1 legacy 2.9 | `--features epd2in9,board_v1` |
| v1 legacy 2.7 | `--features epd2in7,board_v1` |

**Board differences:**

| Item | board_v1 (legacy) | board_v2 (v22 new) |
|------|-------------------|--------------------|
| Buttons | Key 1 = GPIO9 direct digital; keys 2/3 = GPIO2 ADC ladder (doubles as deep-sleep wake) | Keys 1/2/3 all on the GPIO4 ADC ladder (digital input + ADC + deep-sleep wake + PCF INT share one pad) |
| Battery ADC | GPIO4 | GPIO2 |
| Power rails | Dual: EINK=GPIO21 / SD=GPIO1, **high = powered off** (initialized High, pulled low to power on) | Single: GPIO1, **low = powered on** (initialized Low; EINK/SD slots share one rail, set high to cut power in sleep) |
| I2C | none (`init_i2c` returns None) | GPIO9=SDA / GPIO21=SCL, PCF8563(0x51) + AHT20(0x38) |
| PCF8563 | `HAS_PCF=false`: timed wake via ESP RTC; clock falls back to rtc_fast estimation | `HAS_PCF=true`: PCF is preferred for timed wake/clock source |
| AHT20 | `HAS_AHT20=false` | `HAS_AHT20=true` (spawn_sensors starts the sampling task) |
| Pad hold | Before deep sleep, `rtcio_pad_hold` on GPIO1 (SD rail) to keep the rail state during sleep | At boot, clear the `PAD_HOLD` register (releases the hold armed by the previous deep sleep — otherwise GPIO1 can't be driven low → rail loses power → display/SD dead) |

**Shared by both boards (defined in mod.rs):** EPD/SPI/SD pins (busy=6 / rst=7 / dc=20 / cs=3 / sck=8 / mosi=0 / miso=10 / sd_cs=5), `sd_miso_pullup()` (MISO internal pull-up — the SD DO is high-Z before init, and CMD0 responses are unreliable without it; configured before SPI claims GPIO10), and the `RTC_FAST_BOOT_SEQ` diagnostic counter (increments each boot in rtc_fast; a monotonic count across deep sleep = rtc_fast retention working).

### 5.13 pcf8563.rs — Hardware RTC (v2)

**File:** `src/pcf8563.rs`

The PCF8563 (I2C 0x51, CR2032 backup battery) plays two roles: **system clock source** and **deep-sleep timed-wake source**. Time is stored/read as UTC (`Clock::local` applies the +8h offset).

**Timekeeping:** seconds (0x02, bit7=VL invalid flag), minutes, hours, day, weekday, month (bit7=century), year — all BCD.
- `read_time()`: reads 7 bytes and converts to Unix seconds; returns None when VL=1 (backup battery lost power / first power-up) or fields are out of range
- `write_time(ts)`: sets STOP=1 to pause timekeeping before writing (prevents rollback tearing), clears it afterwards
- `ts_valid()`: 2020–2100 range check, consistent with the worldtime module

**Countdown timer** (registers 0x0E Timer_control / 0x0F Timer_A; on reaching 0, TF sets and INT pulls low):
- `arm_timer_secs(n)`: 1Hz tier, INT low after 1..=255 seconds
- `arm_timer_minutes(n)`: 1/60Hz tier, up to 255 minutes (≈4.25 hours), suited to long-period weather refreshes
- `disarm_timer()`: after wake, TE=0 so INT can't pull the button network low and fake a press at runtime
- `consume_timer_wake()`: reads TF/AF to decide if this was a timed wake, then clears flags + stops the timer/interrupt enables

**Absolute-time alarm** (registers from 0x09, minute/hour/day BCD compare + weekday 0x80 to disable): `arm_alarm_at(ts)` sets a UTC moment per the PCF's own timekeeping, for intervals beyond the 255-minute countdown cap. The alarm has no month register: intervals >28 days wake once early on the same day next month (harmless — re-arm on wake).

### 5.14 i2c_bus.rs & aht20.rs (v2)

**`i2c_bus.rs`**: the I2C bus container. `I2C: Mutex<Option<I2cDev>>` holds the bus globally; the `write()` / `write_read()` helpers let the PCF8563 and AHT20 drivers share the bus (mutually exclusive locking). 100kHz, the PCF8563 safe upper bound.

**`aht20.rs`**: temperature/humidity sensor (0x38) driver + periodic sampling task.
- Protocol: wait 40ms after power-up → status byte bit3 = calibrated (otherwise send 0xBE init) → trigger measurement `[0xAC,0x33,0x00]` → wait 80ms → read 7 bytes (status + humidity 20bit + temperature 20bit + CRC8, poly 0x31)
- Integer-only math (no floating point); data lives in atomics (`AtomicI32/AtomicU32`), and the render side reads `indoor_snapshot()` synchronously (lock-free)
- `sample_task`: samples every 60s while awake; the first measurement after a wake often fails (sensor busy) → 3 fast retries 200ms apart; consecutive failures clear the displayed value to avoid staleness

### 5.15 ttf_sd.rs — TTF Vector Font Rendering

**File:** `src/ttf_sd.rs`

Reads TTF glyphs **on demand from the SD card** (no whole-file loading) so reading mode can render arbitrary Chinese fonts:

- Self-implemented TTF parsing chain: table directory → cmap → loca → hmtx → glyf, seeking+reading each glyph from SD
- Quadratic curves rasterized by `ab_glyph_rasterizer` (adaptive flattening + pixel coverage threshold), no_std + libm soft float (riscv32imc has no FPU)
- Caching: `FontInfo` (table offsets/metrics) parsed once and kept in TtfWs, avoiding repeated table reads per page; the full cmap subtable is cached (28KB — cmap is the per-character lookup bottleneck); loca/hmtx keep small caches while glyf falls back to SD; the glyph bitmap cache is a flat buffer (12KB budget, split into slots by font size at runtime — large sizes get fewer larger slots, small sizes more smaller slots)
- The `TtfWs` workspace is **heap-allocated**: created on entering reading, freed on exit, mutually exclusive with WiFi heap usage at runtime (reading mode has no network), no longer permanently resident in .bss
- Stroke bolding factor (×1.0~×3.0, pixel coverage × factor test): fixes thin-stroke gaps; adjustable in the reading typography menu and persisted

---

## 6. Data Model Layer

**Directory:** `src/model/`

| File | Purpose |
|------|---------|
| `seniverse.rs` | Seniverse weather API structures, wind direction conversion, Beaufort scale |
| `open_meteo.rs` | Open-Meteo API conversion (WMO → Seniverse format) |
| `holiday.rs` | Holiday structures (date, name, is-off-day) |
| `lunar.rs` | Lunar/solar term/zodiac algorithms (1900–2100 precomputed tables, leap months) |
| `stock.rs` | Stock model: `KLine`/`ChartMode` (6-mode ring)/`RealtimeQuote`/`StockMinuteCache` (intraday cache), Sina K-line and Tencent quote parsing, URL building, price formatting |

---

## 7. UI Widget Layer

**Directory:** `src/widgets/`

All widgets build on the `embedded-graphics` `Drawable` trait and can draw to any `DrawTarget`.

| Widget | File | Purpose |
|--------|------|---------|
| IconGridWidget | `icon_grid_widget.rs` | Icon grid menu (home page) |
| ListWidget | `list_widget.rs` | Scrollable list (bookmarks, file lists) |
| Calendar | `calendar.rs` | Calendar grid (Gregorian + lunar + solar terms + holidays) |
| WeatherIcon | `weather_icon.rs` | Weather icons (~20 types, 32×32 BMP) |
| TempChart | `temp_chart.rs` | Temperature trend chart (high/low dual line) |
| KLine | `kline.rs` | Stock candlestick/line chart (hollow/solid up/down, window clipping) |
| QrcodeWidget | `qrcode_widget.rs` | QR code generation and display |
| Battery | `battery.rs` | Battery icon + percentage |
| draw_icon | `draw_icon.rs` | Status icons (wifi / loading / moon) |
| ScrollBar | `scroll_bar.rs` | Scroll bar (horizontal/vertical, used by ListWidget) |

---

## 8. Page System

**Directory:** `src/pages/`

### 8.1 Page Trait

```rust
pub trait Page {
    fn new() -> Self;
    async fn render(&mut self);
    async fn run(&mut self, spawner: Spawner);
    async fn bind_event(&mut self);
}
```

### 8.2 Page Routing

`MainPage` keeps the menu list and the current selection, routing through `PageEnum`. Normal mode is driven by `main_task`; reading mode has its own entry `reading_task` (spawned directly by main, see 5.1.10):

```
MainPage.run()
  ├── current_page == None → show the icon grid menu
  └── current_page == Some(index)
        match menus[index].page_enum {
            EReadPage → ReadPage::new().bind_event().run()
            EWeatherPage → WeatherPage::new().bind_event().run()
            ECalendarPage → CalendarPage::new().bind_event().run()
            EStockPage → StockPage::new().bind_event().run()
            EImageListPage → ImagePage::new().bind_event().run()
            ESettingPage → SettingPage::new().bind_event().run()
            EDebugPage → DebugPage::new().bind_event().run()
        }
        → on return, bind_event() rebinds the main menu events

reading_task (reading restart split-mode)
  → wait for the display task → ReadPage::new().bind_event().run()
  → on exit: clear READING_MODE + set the grid page to -1 → reboot_sleep() back to normal mode
```

### 8.3 Page Functions

| Page | Key mapping | Main functions |
|------|-------------|----------------|
| **MainPage** | Key1/2 up/down, Key3 confirm | Icon grid navigation; cold boot enters the default home page, wake returns to the last page (rtc_fast + NVS fallback) |
| **ReadPage** | Key1 next page/down, Key2 previous page/up (swappable via "key swap"), Key3 short = menu/confirm, long-3 exit, long 1/2 auto-repeat | TTF vector reading (SD fonts selectable) + built-in bitmap fonts; TTF dynamic pagination (O(1) forward, history stack backward); two-level book list; menu: back to list / bookmarks / jump to progress / typography / fonts / rotate / key swap / sleep; bookmark list + open/delete popup; typography (size 12~40 / line gap ±10 / stroke bolding / reset); three-decimal progress; reading position persisted to `.log[0]` |
| **WeatherPage** | Key1 refresh weather, Key2 refresh holidays, long-3 exit | Overview, 5-day forecast, temperature chart, indoor temperature & humidity (v2); sleep duration: show-time on → aligns to the next minute boundary (wake lands on a minute tick), off → sleeps until the weather data expires (aligned to the 5h sync interval) |
| **CalendarPage** | Key1/2 short = change month, Key3 short = back to today, long-1 refresh weather, long-2 refresh holidays, long-3 exit | Calendar, lunar dates, solar terms, holidays (rest/work) |
| **StockPage** | Short 1/2: move cursor (overview) / switch chart mode (detail); long-1: force refresh (overview) / previous stock (detail); long-2: next cell (overview) / next stock (detail); short 3: enter detail / re-request; long-3: back level by level | Overview (6-stock intraday cache grid) / detail: intraday / daily K / weekly K / monthly K / line / board; realtime mode refreshes every 120s during market hours; outside market hours (lunch/pre/post/weekends/holidays) sleeps until the next open; other modes every 12h |
| **ImagePage** | Key1/2 select (long = auto-repeat), Key3 view/menu, long-3 exit | BMP browsing and wallpaper setting |
| **SettingPage** | Key1/2 adjust (long = auto-repeat), Key3 confirm, long-3 exit | Reading sleep timeout, weather sleep timeout, auto location, web config, stock selection, default home, full-refresh count, manual time sync, holiday sync, show time |
| **DebugPage** | Key3 exit, Key1 long = clear | Error log viewer |

---

## 9. Key Design Patterns

### 9.1 SPI Bus Sharing

```
SPI2 master, 32MHz, Mode 0
  │
  CsMutex<RefCell<Spi>>
  ├── CriticalSectionDevice(CS=GPIO3) → EPD     (new_no_delay)
  └── CriticalSectionDevice(CS=GPIO5) → SD Card  (new_no_delay)
```

Each SPI operation automatically disables interrupts for mutual exclusion; `new_no_delay` replaces the old constructor that required a `Delay`.

### 9.2 Event-Driven Architecture

```
Button GPIO falling edge → event::run() → key_detection() → toggle_event()
  → EVENT_QUEUE (FIFO, capacity 8, KeyLongIng tail-merge, drop-oldest when full) + EVENT_READY
  → event::dispatch() task pops events one by one → Listener callbacks run serially
  → page state changes → render() → RENDER_CHANNEL → display::render()
```

Producer (scanning) and consumer (callback execution) are decoupled: scanning keeps running while a long callback executes, so no presses are lost.

### 9.3 Global Static State

```rust
// Pattern 1: Embassy Mutex + Option
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);

// Pattern 2: core::ptr safe access to static mut
unsafe { core::ptr::addr_of_mut!(DISPLAY).write(Some(display)); }
unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut() }

// Pattern 3: StaticCell + make_static!
let shared_spi_static = static_cell::make_static!(shared_spi);

// Pattern 4: RTC fast memory (preserved across deep sleep)
#[ram(unstable(rtc_fast))]
static mut PAGE_INDEX: i32 = 1;

// Pattern 5: atomics as lock-free snapshots (AHT20 indoor readings for the render path)
static INDOOR_T: AtomicI32 = AtomicI32::new(i32::MIN);
```

### 9.4 Conditional Compilation

```rust
// Display dimension: layout/pagination parameters per feature
#[cfg(feature = "epd2in9")]
const LINES_NUM: u32 = 7;

#[cfg(feature = "epd4in2")]
const LINES_NUM: u32 = 22;

// Board dimension: button task entry / ADC type / sensors / PCF
#[cfg(feature = "board_v2")]
pub async fn run(key2: crate::board::KeyLadderGpio) { /* single ladder */ }

#[cfg(feature = "board_v1")]
pub async fn run(key1: GPIO9<'static>, key2: crate::board::KeyLadderGpio) { /* dual path */ }

// Data-source dimension
#[cfg(not(feature = "weather-openmeteo"))]
{ Self::request_seniverse(&weather_storage).await }

#[cfg(feature = "weather-openmeteo")]
{ Self::request_open_meteo(&weather_storage).await }
```

### 9.5 Channel / Queue Decoupling

| Channel | Producer | Consumer | Data |
|---------|----------|----------|------|
| `EVENT_QUEUE` + `EVENT_READY` | event::run (button scan) | event::dispatch | `EventType` FIFO |
| `RENDER_CHANNEL` | pages | `display::render` | `RenderInfo` |
| `QUICKLY_LUT_CHANNEL` | pages | `display::render` | `bool` |
| `STOP_WIFI_SIGNAL` | sleep/do_stop | connection_wifi | `()` |
| `RECONNECT_WIFI_SIGNAL` | use_wifi | connection_wifi | `()` |
| `REINIT_WIFI_SIGNAL` | use_wifi | connect_wifi | `()` |

---

## 10. Data Flow

### 10.1 Button Event Flow

```
Physical button → GPIO falling edge → event::run() → (ADC distinguishes key number) → key_detection()
  → toggle_event() → EVENT_QUEUE (FIFO)
  → event::dispatch() dispatches one by one → Listener callbacks (serial)
  → page state changes → render() → RENDER_CHANNEL → display::render() → EPD
```

### 10.2 Network Request Flow

```
ntp_worker / weather trigger → use_wifi() (lock) → get Stack
  → RequestClient::new(stack) → send_request(url)
  → (HTTP: reqwless | HTTPS: embedded-tls) → parse JSON
  → save to Flash → finish_wifi() (unlock)
```

### 10.3 Render Pipeline

```
Page → display_mut() framebuffer → clear_buffer(White)
  → embedded-graphics drawing (Text/Rectangle/Widget)
  → RENDER_CHANNEL.send() → display::render() receives
  → EPD wake → refresh count check → Quick/Full refresh → EPD sleep
```

### 10.4 Timed-Wake Data Flow

```
Sleep (page sleep_time>0) → sleep::to_sleep_tips tiered arming (PCF countdown/alarm, ESP RTC fallback)
  → deep sleep → PCF INT pulls the key-ladder GPIO low (or the ESP RTC fires) → wake & restart
  → main: consume_timer_wake() classifies → set_timed_wake(8s window if stale, else 0)
  → page run: within the hold-off window wait for WiFi/sync/AHT20 → refresh + render done
  → user hasn't touched buttons → to_sleep_tips sleeps immediately ("sleep right after refresh")
```

---

## 11. Memory Management

### 11.1 Memory Layout

```
ESP32-C3 memory (~400KB SRAM)
  │
  ├── Global heap (~64KB)    dedicated dram2 section (.dram2_uninit, bootloader-released DRAM)
  │     registered via esp_alloc::HEAP.add_region(...)
  │     used for Vec, String, Box, etc. (WiFi ~60KB / TtfWs ~44KB reuse the same block)
  │
  ├── Main DRAM              entirely left for task stacks (deep TTF render call chains once overflowed)
  │
  ├── Embassy task stacks     allocated by the executor
  │
  ├── Statics (.bss + .data)  global Mutexes, Channels, Signals, etc.
  │
  └── RTC fast memory         #[ram(unstable(rtc_fast))]
        variables powered during sleep (page index/resume offset/mode flags/render
        counts/intraday cache, etc.)
```

### 11.2 heapless Collections

Fixed-capacity collections prefer `heapless`, avoiding heap fragmentation:

```rust
heapless::String<32>     // fixed 32-byte string
heapless::Vec<T, 20>     // fixed 20-element Vec
```

---

## 12. Async Architecture

### 12.1 Embassy Task List

| Task | Spawned in | Responsibility |
|------|-----------|----------------|
| `display::render` | main.rs | EPD render loop |
| `event::run` | main.rs | Button detection and event enqueue (two per-board entry points) |
| `event::dispatch` | main.rs | FIFO serial dispatch of the event queue to listener callbacks |
| `battery::test_bat_adc` | main.rs | Battery ADC sampling (60s cycle) |
| `worldtime::ntp_worker` | main.rs | NTP sync (writes PCF8563) + weather/holiday sync |
| `aht20::sample_task` | board::spawn_sensors (v2 only) | Indoor temperature/humidity sampling (60s cycle) |
| `pages::main_task` | main.rs | Main UI event loop (normal mode) |
| `pages::reading_task` | main.rs (reading mode only) | Reading-mode entry (owns the heap for TTF) |
| `connection_wifi` | wifi.rs | WiFi STA connection state management |
| `net_task` | wifi.rs | Network stack Runner |
| `do_stop` | wifi.rs | WiFi auto shutdown (30s timeout) |
| `dhcp_service` | wifi.rs | AP-mode DHCP service |
| `dns_service` | wifi.rs | AP-mode DNS hijacking |
| `connection_wifi_ap` | wifi.rs | AP-mode connection management |

### 12.2 Task Interaction Diagram

```
                    ┌─────────────┐
                    │   main.rs   │
                    └──────┬──────┘
                           │ spawn
           ┌───────────────┼───────────────┐
           ▼               ▼               ▼
    ┌────────────┐  ┌────────────┐  ┌────────────┐
    │ event::run │  │display::   │  │ntp_worker  │
    │ (scan)     │  │render      │  │            │
    └─────┬──────┘  └──────┬─────┘  └─────┬──────┘
          │ toggle_event   │               │ use_wifi()
          ▼                │               ▼
    ┌────────────┐         │        ┌────────────┐
    │ event::    │         │        │connection_ │
    │ dispatch   │         │        │wifi        │
    └─────┬──────┘         │        └──────┬─────┘
          │ callbacks      │               │
          ▼                ▼               ▼
    ┌────────────┐  ┌────────────┐  ┌────────────┐
    │pages::     │  │ EPD        │  │ net_task   │
    │main_task / │  │ hardware   │  │ (Runner)   │
    │reading_task│  └────────────┘  └────────────┘
    └────────────┘
```

### 12.3 Cooperative Scheduling

Embassy schedules cooperatively; tasks yield via `.await`:

- `Timer::after(...).await` — timed wait
- `channel.receive().await` — wait for a message
- `mutex.lock().await` — wait for a lock
- `select(a, b).await` — await whichever future completes first

---

## 13. Pin Assignment

### 13.1 Pins Shared by Both Boards (EPD / SPI / SD)

| GPIO | Direction | Function | Notes |
|------|-----------|----------|-------|
| GPIO0 | Output | EPD MOSI (SPI) | SPI data out |
| GPIO3 | Output | EPD CS (SPI chip select) | High = not selected |
| GPIO5 | Output | SD card CS (SPI chip select) | High = not selected |
| GPIO6 | Input | EPD BUSY | Display busy state |
| GPIO7 | Output | EPD RST | Display reset |
| GPIO8 | Output | EPD SCLK (SPI clock) | SPI clock line |
| GPIO10 | Input | EPD/SD MISO (SPI) | Internal pull-up (SD DO is high-Z before init) |
| GPIO20 | Output | EPD DC (data/command) | SPI DC signal |

### 13.2 board_v1 (Legacy Board)

| GPIO | Direction | Function | Notes |
|------|-----------|----------|-------|
| GPIO1 | Output | SD card power control | High = powered off (pad hold armed before sleep to keep state) |
| GPIO2 | Input | Keys 2/3 (ADC ladder) | Multi-key ADC detection, doubles as deep-sleep wake (Low) |
| GPIO4 | Input | Battery ADC | Battery voltage divider sensing |
| GPIO9 | Input | Key 1 | Direct digital, pull-up input |
| GPIO21 | Output | E-paper power control | High = powered off |

### 13.3 board_v2 (v22 New Board)

| GPIO | Direction | Function | Notes |
|------|-----------|----------|-------|
| GPIO1 | Output | Single power rail (EINK + SD) | Low = powered on, set high to cut power in sleep; pad hold released at boot |
| GPIO2 | Input | Battery ADC | Divider sensing; deep-sleep isolated (slp_sel=1 + IE/WPU/WPD=0) against leakage |
| GPIO4 | Input | Keys 1/2/3 (ADC ladder) | Digital input + ADC + deep-sleep wake + PCF INT share one pad |
| GPIO9 | I/O | I2C SDA | PCF8563 (0x51) + AHT20 (0x38) |
| GPIO21 | I/O | I2C SCL | 100kHz (PCF8563 safe upper bound) |

**SPI configuration:** SPI2 master, 32MHz, Mode 0 (CPOL=0, CPHA=1)

---

## 14. Appendix: esp-hal Upgrade Notes

### 14.1 Dependency Migration Table

| Function | Old crate | New crate | Key change |
|----------|-----------|-----------|------------|
| HAL | esp-hal 0.19 | esp-hal 1.1 | Full API rewrite |
| WiFi | esp-wifi 0.7 | esp-radio 0.18 | Driver split, new init model |
| Embassy integration | esp-hal-embassy 0.2 | esp-rtos 0.3 | Unified runtime entry |
| Memory allocation | embedded-alloc | esp-alloc 0.10 | Macro simplified, auto-managed |
| Boot | *(none)* | esp-bootloader-esp-idf 0.5 | IDF-compatible description |
| Network | embassy-net 0.4 | embassy-net 0.9 | Stack/Runner split |
| Executor | embassy-executor 0.6 | embassy-executor 0.10 | task-arena-size removed |
| Sync | embassy-sync 0.6 | embassy-sync 0.8 | Minor API changes |
| Time | embassy-time 0.3 | embassy-time 0.5 | Minor API changes |
| SPI sharing | embedded-hal-bus 0.1 | embedded-hal-bus 0.3 | new_no_delay |

### 14.2 Code Pattern Changes

| Pattern | Old | New |
|---------|-----|-----|
| Peripheral access | `Peripherals::take()` | `hal_init(HalConfig)` |
| Clocks | `ClockControl::max().freeze()` | `CpuClock::max()` |
| GPIO | `Io::new(peripherals.GPIO, peripherals.IO_MUX)` | `peripherals.GPIO6` |
| Output | `Output::new(pin, Level::High)` | `Output::new(pin, Level::High, OutputConfig::default())` |
| Input | `Input::new(pin, Pull::Up)` | `Input::new(pin, InputConfig::default().with_pull(Pull::Up))` |
| SPI | `Spi::new(spi2, freq, mode, &clocks)` | `Spi::new(spi2, SpiConfig::default().with_frequency(Rate::from_mhz(32)).with_mode(Mode::_0))` |
| SPI sharing | `CriticalSectionDevice::new(mutex, cs, delay)` | `CriticalSectionDevice::new_no_delay(mutex, cs)` |
| RNG | `Rng::new(peripherals.RNG)` | `Rng::new()` |
| RTC memory | `#[ram(rtc_fast)]` | `#[ram(unstable(rtc_fast))]` |
| Static mut | `unsafe { DISPLAY.replace(x) }` | `unsafe { core::ptr::addr_of_mut!(DISPLAY).write(x) }` |
| WiFi init | `esp_wifi::initialize(...) + new_with_mode()` | `esp_radio::wifi::new(wifi, config)` |
| Network | `Stack::new(iface, config, res, seed)` | `embassy_net::new(iface, config, res, seed)` → `(Stack, Runner)` |
| WiFi events | `controller.wait_for_event(Event)` | `controller.subscribe() + next_event_pure()` |
| Backtrace | `esp_backtrace::arch::backtrace()` | `Backtrace::capture().frames()` |
| Flash | `FlashStorage::new()` | `FlashStorage::new(FLASH::steal())` |
| Network addresses | `esp_wifi::wifi::ipv4::*` | `no_std_net::*` |
| Async trait | `async fn send_to(...)` | `fn send_to(...) -> impl Future<...>` |
| Heap allocation | manual `embedded_alloc::Heap` + `init()` | dram2 section + `esp_alloc::HEAP.add_region(...)` |
| Entry macro | `#[esp_hal_embassy::main]` | `#[esp_rtos::main]` |
| Task spawn | `spawner.spawn(task(...)).ok()` | `spawner.spawn(task(...).unwrap())` |
| Stack type | `Stack<WifiDevice<'static, WifiStaDevice>>` | `Stack<'static>` |

---

## Appendix: Build & Flash

```bash
# Install the toolchain
rustup target add riscv32imc-unknown-none-elf

# Install espflash
cargo install espflash

# Build & flash the v22 new board 4.2" version (.cargo/config.toml sets espflash as runner)
cargo run --release --features epd4in2,board_v2

# Build the v1 legacy board 4.2 / 2.7 / 2.9 versions
cargo run --release --features epd4in2,board_v1
cargo run --release --features epd2in7,board_v1
cargo run --release --features epd2in9,board_v1

# With debug mode (enters the debug page if an error log exists in Flash at boot)
cargo run --release --features "epd4in2,board_v2,enable_debug"
```

> `--release` is mandatory (`[profile.release] opt-level = "s"`); otherwise `.text` overflows the 3MB factory partition.
