# EPD Reader 详细设计文档

> 基于 Rust 的 ESP32-C3 电子墨水屏阅读器（esp-hal v1.x）

---

## 目录

1. [项目概述](#1-项目概述)
2. [系统架构总览](#2-系统架构总览)
3. [构建系统与配置](#3-构建系统与配置)
4. [模块结构](#4-模块结构)
5. [核心模块详解](#5-核心模块详解)
6. [数据模型层](#6-数据模型层)
7. [UI 组件层](#7-ui-组件层)
8. [页面系统](#8-页面系统)
9. [关键设计模式](#9-关键设计模式)
10. [数据流](#10-数据流)
11. [内存管理](#11-内存管理)
12. [异步架构](#12-异步架构)
13. [引脚分配表](#13-引脚分配表)
14. [附录：esp-hal 升级要点](#14-附录esp-hal-升级要点)

---

## 1. 项目概述

### 1.1 项目简介

epd-reader 是一个运行在 **ESP32-C3** 上的嵌入式电子墨水屏（E-Paper Display）阅读器，使用 Rust 语言开发。项目实现了电子书阅读、天气查询、农历日历、图片浏览、WiFi 配网等功能，是一个功能完整的嵌入式应用。

本项目基于 **esp-hal v1.x** 生态构建，采用最新的 `esp-radio` 无线驱动、`esp-rtos` 异步运行时集成、`esp-alloc` 内存管理器，代表了 2025 年 Rust 嵌入式 ESP32 开发的最新实践。

### 1.2 功能列表

| 功能 | 说明 |
|------|------|
| 电子书阅读 | 支持 TXT 文件，自动分页、书签管理、进度显示 |
| 天气显示 | 心知天气 / Open-Meteo 双数据源，5 日预报 + 温度曲线 |
| 自动定位 | 基于 IP 反查城市与经纬度，自动设置天气查询地点 |
| 农历日历 | 公历/农历对照、节气、节假日显示（含补班标记） |
| 股票行情 | 沪深股票分时 / 日K / 周K / 月K / 折线 + 实时盘口，多股切换、按周期自动刷新 |
| 图片浏览 | 支持黑白 BMP 图片查看，可设为待机壁纸 |
| WiFi 配网 | AP 模式热点 + Web 配置界面，手机扫码即可配网 |
| NTP 时间 | 自动同步网络时间，深度睡眠唤醒后通过 RTC 恢复 |
| 电池管理 | ADC 采样电池电压，电量百分比显示 |
| 低功耗 | 自动深度睡眠，按键/定时器唤醒，外设电源独立控制 |
| 错误日志 | 自定义 Panic Handler，崩溃信息写入 Flash，重启后可查看 |

### 1.3 硬件平台

| 组件 | 型号/规格 |
|------|-----------|
| MCU | ESP32-C3 (RISC-V, 160MHz, 400KB SRAM) |
| 显示屏 | Waveshare 4.2" (400×300) 或 2.7" (264×176) 电子墨水屏（2.9" 驱动层有适配但页面布局缺失，暂不可用） |
| 存储 | MicroSD 卡（通过 SPI 连接，≤32GB SD/SDHC，MBR 分区） |
| Flash | 内置 4MB Flash |
| 输入 | 按键 ×3（GPIO9 独立键 + GPIO2 经 ADC 分压实现两键） |
| 电源 | 锂电池 + ADC 电压检测 |

### 1.4 技术栈

| 类别 | 技术 | 用途 |
|------|------|------|
| 语言 | Rust (Edition 2024, nightly) | `#![no_std]` 嵌入式环境 |
| 目标平台 | `riscv32imc-unknown-none-elf` | ESP32-C3 RISC-V 核心 |
| HAL | esp-hal v1.1.0 | 硬件抽象层 |
| RTOS 集成 | esp-rtos v0.3.0 | Embassy 异步运行时集成 |
| WiFi | esp-radio v0.18.0 | 无线网络驱动 |
| 网络栈 | embassy-net v0.9.0 | TCP/IP 协议栈 |
| 异步运行时 | embassy-executor v0.10.0 | 异步任务调度 |
| 内存 | esp-alloc v0.10.0 | 全局堆分配器 |
| 图形 | embedded-graphics v0.8 | 帧缓冲绘制框架 |
| 屏幕驱动 | epd-waveshare (自定义 fork) | 电子墨水屏底层驱动 |
| 字体 | u8g2-fonts v0.7.1 | 中文字体渲染（GB2312） |
| 文件系统 | embedded-sdmmc v0.9.0 | SD 卡 FAT 文件系统 |
| 存储 | esp-storage v0.9.0 | ESP32 内部 Flash 读写 |
| HTTP | reqwless v0.14.0 | HTTP 客户端 |
| TLS | embedded-tls v0.18 | TLS 加密连接 |
| JSON | mini-json (自定义库) | 轻量 JSON 解析 |
| 网络地址 | no-std-net v0.6 | 无 std 网络地址类型 |

---

## 2. 系统架构总览

### 2.1 整体架构图

```
┌─────────────────────────────────────────────────────────────────┐
│                        应用层 (Pages)                            │
│  MainPage ─┬─ ReadPage (电子书)   ─┬─ WeatherPage (天气)         │
│            ├─ CalendarPage (日历)   ├─ ImagePage (图片)          │
│            ├─ StockPage (股票)      ├─ SettingPage (设置)         │
│            └─ DebugPage (调试)                                   │
├─────────────────────────────────────────────────────────────────┤
│                      UI 组件层 (Widgets)                         │
│  IconGridWidget │ ListWidget │ Calendar │ TempChart │ KLine       │
│  WeatherIcon    │ QrcodeWidget│ Battery │ draw_icon │ ScrollBar  │
├─────────────────────────────────────────────────────────────────┤
│                      服务层 (Services)                           │
│  Display (渲染) │ Event (事件) │ WiFi (网络) │ Storage (持久化)   │
│  WorldTime (NTP)│ Weather (天气)│ Battery (电池)│ Sleep (睡眠)   │
│  Request (HTTP) │ WebService (配置)│ SDMount (文件) │ TxtReader   │
│  Location (IP 定位)                                             │
├─────────────────────────────────────────────────────────────────┤
│                      数据模型层 (Model)                          │
│  seniverse (天气) │ open_meteo (天气) │ lunar (农历/节气/星座)    │
│  holiday (节假日) │ stock (K线/盘口)                              │
├─────────────────────────────────────────────────────────────────┤
│                    ESP 生态 (esp-rs v1.x)                        │
│  esp-hal │ esp-radio │ esp-storage │ esp-alloc │ esp-rtos       │
│  esp-bootloader-esp-idf │ esp-backtrace │ esp-println           │
├─────────────────────────────────────────────────────────────────┤
│                      异步运行时 (Embassy)                        │
│  embassy-executor v0.10 │ embassy-time v0.5 │ embassy-sync v0.8 │
│  embassy-net v0.9 │ embassy-futures                             │
├─────────────────────────────────────────────────────────────────┤
│                      硬件 (Hardware)                             │
│  ESP32-C3 │ E-Paper Display │ SD Card │ Buttons │ Battery       │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 模块依赖关系

```
main.rs
 ├── display.rs ◄── epd-waveshare, embedded-graphics, critical-section
 ├── event.rs   ◄── embassy-sync, embassy-futures, esp-hal (GPIO/ADC)
 ├── pages/     ◄── event, display, widgets, model
 │   ├── main_page.rs      (页面管理器/默认主页/唤醒回上次页)
 │   ├── read/             ◄── epd2in9_txt, sd_mount（layout 按 feature 选尺寸）
 │   ├── weather/          ◄── weather, worldtime, location
 │   ├── calendar/         ◄── weather, worldtime, widgets::calendar, model::lunar
 │   ├── stock/            ◄── request, model::stock, widgets::kline, wifi
 │   ├── image_page.rs     ◄── sd_mount, flash_sleep
 │   ├── setting_page.rs   ◄── wifi, web_service, storage, location
 │   └── debug_page.rs     ◄── storage
 ├── wifi.rs    ◄── esp-radio, embassy-net, dhcparse
 ├── storage.rs ◄── esp-storage
 ├── weather.rs ◄── request, storage, worldtime, model::{seniverse,open_meteo,holiday}
 ├── location.rs ◄── request, wifi, mini-json
 ├── worldtime.rs ◄── sntpc, embassy-net, no-std-net
 ├── request.rs ◄── reqwless, embedded-tls
 ├── battery.rs ◄── esp-hal (ADC)
 ├── sleep.rs   ◄── esp-hal (RTC)
 ├── epd2in9_txt.rs ◄── embedded-sdmmc
 ├── sd_mount.rs ◄── embedded-sdmmc
 ├── flash_sleep.rs ◄── esp-storage, embedded-graphics
 ├── web_service.rs ◄── wifi, embassy-net (TCP)
 ├── panic.rs   ◄── esp-backtrace, storage
 ├── model/     ◄── mini-json
 └── widgets/   ◄── embedded-graphics, u8g2-fonts
```

---

## 3. 构建系统与配置

### 3.1 工具链配置 (`rust-toolchain.toml`)

```toml
[toolchain]
targets = ["riscv32imc-unknown-none-elf"]
# channel 未固定，使用默认 nightly
```

**说明：**
- ESP32-C3 使用 RISC-V 32 位指令集（`riscv32imc`），无 MMU，跑不了 Linux 这类标准操作系统。不过 Rust 仍可用 ESP-IDF 的 std 模式（`riscv32imc-esp-espidf`）或 esp-hal 裸机模式；本项目选后者，所以目标是 `riscv32imc-unknown-none-elf`
- 需要 nightly Rust 以使用 `generic_const_exprs`、`type_alias_impl_trait` 等不稳定特性
- esp-hal v1.x 对 nightly 兼容性更好，不再需要锁定特定日期版本

### 3.2 Cargo 配置 (`.cargo/config.toml`)

```toml
[target.riscv32imc-unknown-none-elf]
runner = "espflash flash --chip esp32c3 --baud 115200 --monitor --partition-table ./partitions.csv"

[build]
rustflags = [
  "-C", "linker-arg=-Tlinkall.x",
  "-C", "force-frame-pointers",
]
target = "riscv32imc-unknown-none-elf"
```

**与旧版差异：**
- 移除了 `-Trom_functions.x` 链接脚本（esp-hal v1.x 不再需要）
- 保留 `linkall.x` 内存布局链接脚本
- `force-frame-pointers` 保留用于 backtrace 调试

### 3.3 Flash 分区表 (`partitions.csv`)

```csv
# Name,   Type, SubType,  Offset,   Size, Flags
nvs,      data, nvs,      ,         0x6000,    ← 非易失性存储 (24KB)
phy_init, data, phy,      ,         0x1000,    ← PHY 初始化数据 (4KB)
factory,  app,  factory,  ,         3M,        ← 应用程序 (3MB)
storage,  data, fat,      ,         128K,      ← FAT 文件系统 (128KB)
```

- `nvs` 分区（`0x9000` 起始）存储 WiFi 配置、天气数据、设置项等
- `storage` 分区（`0x310000` 起始）存储待机壁纸图片

### 3.4 Feature Flags (`Cargo.toml`)

```toml
[features]
epd2in9 = []           # 2.9 寸屏幕 (296×128) — 仅驱动层适配，页面布局缺失，暂不可编译
epd2in7 = []           # 2.7 寸屏幕 (264×176)
epd4in2 = []           # 4.2 寸屏幕 (400×300)
enable_debug = []      # 启动时若 Flash 有错误日志则进入调试页
weather-openmeteo = [] # 天气数据源切换为 Open-Meteo（默认心知天气）
```

```bash
cargo run --release --features epd4in2               # 构建 4.2 寸版本
cargo run --release --features epd2in7               # 构建 2.7 寸版本
cargo run --release --features "epd4in2,enable_debug" # 带调试模式
```

> 固件体积已超过 debug 默认配置，必须用 `--release` 才能链接进 3MB factory 分区。`epd2in9` 缺页面布局，暂不可编译。

### 3.5 核心依赖说明

```toml
# ── ESP 生态 v1.x ──
esp-hal = { version = "1.1.0", features = ["esp32c3", "unstable", "log-04"] }
esp-radio = { version = "0.18.0", features = ["esp32c3", "wifi", "log-04", "unstable"] }
esp-rtos = { version = "0.3.0", features = ["esp32c3", "embassy", "esp-radio", "log-04"] }
esp-alloc = { version = "0.10.0", features = ["esp32c3", "global-allocator"] }
esp-storage = { version = "0.9.0", features = ["esp32c3"] }
esp-bootloader-esp-idf = { version = "0.5.0", features = ["esp32c3"] }
esp-backtrace = { version = "0.19.0", features = ["esp32c3", "println"] }
esp-println = { version = "0.17.0", features = ["esp32c3", "log-04"] }

# ── Embassy 异步框架 ──
embassy-executor = { version = "0.10.0", features = ["nightly"] }
embassy-time = "0.5.0"
embassy-net = { version = "0.9.0", features = ["dhcpv4", "medium-ethernet", "tcp", "dns", "udp"] }
embassy-sync = "0.8"

# ── 图形与显示 ──
embedded-graphics = "0.8"
epd-waveshare = { git = "..." }
u8g2-fonts = { version = "0.7.1" }

# ── 存储 ──
embedded-sdmmc = "0.9.0"

# ── 网络 ──
reqwless = "0.14.0"
embedded-tls = "0.18"
no-std-net = "0.6"
sntpc = "0.3"
```

**与旧版主要变化：**

| 旧版 (esp-hal 0.19) | 新版 (esp-hal 1.x) | 说明 |
|---------------------|---------------------|------|
| `esp-hal` 0.19 | `esp-hal` 1.1.0 | HAL 主版本升级 |
| `esp-wifi` 0.7 | `esp-radio` 0.18 | WiFi 驱动重命名重写 |
| `esp-hal-embassy` 0.2 | `esp-rtos` 0.3 | RTOS 集成层 |
| `embedded-alloc` | `esp-alloc` 0.10 | ESP 专用分配器 |
| *(无)* | `esp-bootloader-esp-idf` 0.5 | 新增引导加载器支持 |
| `embassy-executor` 0.6 | `embassy-executor` 0.10 | 任务调度器升级 |
| `embassy-net` 0.4 | `embassy-net` 0.9 | 网络栈重大升级 |
| `embassy-sync` 0.6 | `embassy-sync` 0.8 | 同步原语升级 |
| `embedded-hal-bus` 0.1 | `embedded-hal-bus` 0.3 | HAL 总线升级 |
| `reqwless` 0.11 | `reqwless` 0.14 | HTTP 客户端升级 |
| *(无)* | `no-std-net` 0.6 | 新增网络地址类型 |
| `[patch.crates-io]` 大量补丁 | 无补丁 | 直接使用上游 crate |
| edition = "2021" | edition = "2024" | Rust 版本升级 |

---

## 4. 模块结构

```
src/
├── main.rs              # 程序入口，硬件初始化，主循环
├── panic.rs             # 自定义 panic 处理器（崩溃写 Flash）
├── display.rs           # 电子墨水屏渲染服务（独立 Embassy task）
├── event.rs             # 事件系统（按键检测 + 发布-订阅）
├── sleep.rs             # 深度睡眠与唤醒管理
├── wifi.rs              # WiFi STA/AP 模式（基于 esp-radio）
├── worldtime.rs         # NTP 时间同步，Clock 全局时钟
├── weather.rs           # 天气/节假日数据请求与缓存（双数据源）
├── location.rs          # 基于 IP 的自动定位（ip-api）
├── request.rs           # HTTP/HTTPS GET 客户端
├── battery.rs           # 电池电压 ADC 采样
├── storage.rs           # Flash 持久化存储（NVS 区域）
├── sd_mount.rs          # SD 卡挂载与文件操作封装（含手动 LFN）
├── epd2in9_txt.rs       # TXT 文本分页引擎（多屏尺寸）
├── flash_sleep.rs       # 待机壁纸的 Flash 存储与渲染
├── web_service.rs       # WiFi 配网的 Web 服务
├── random.rs            # 硬件 RNG → rand_core 封装
├── utils.rs             # make_static! 宏
│
├── model/               # 数据模型层
│   ├── mod.rs
│   ├── seniverse.rs     # 心知天气 API 数据结构
│   ├── open_meteo.rs    # Open-Meteo API 数据转换
│   ├── holiday.rs       # 节假日数据结构
│   ├── lunar.rs         # 农历/节气/星座算法
│   └── stock.rs         # 股票模型（KLine/ChartMode/RealtimeQuote + 解析）
│
├── widgets/             # UI 组件层
│   ├── mod.rs
│   ├── icon_grid_widget.rs  # 图标网格菜单
│   ├── list_widget.rs       # 可滚动列表
│   ├── weather_icon.rs      # 天气图标（BMP）
│   ├── temp_chart.rs        # 温度趋势图
│   ├── calendar.rs          # 日历组件（含农历/节气/节假日）
│   ├── kline.rs             # 股票蜡烛图/折线
│   ├── qrcode_widget.rs     # 二维码组件
│   ├── battery.rs           # 电池图标
│   ├── draw_icon.rs         # 状态图标（wifi/加载/月亮）
│   └── scroll_bar.rs        # 滚动条
│
└── pages/               # 页面层（每个子目录含 mod.rs/page.rs/layout*.rs）
    ├── mod.rs               # Page trait 定义，main_task，PageEnum
    ├── main_page.rs         # 主菜单页面（页面管理器/默认主页/唤醒回上次页）
    ├── debug_page.rs        # 错误日志调试页
    ├── image_page.rs        # 图片浏览页面
    ├── setting_page.rs      # 设置/配网页面
    ├── read_menu_page.rs    # 占位（todo!()，未使用）
    ├── read/                # 电子书阅读（layout 按 feature 选尺寸）
    ├── weather/             # 天气页面（layout_264x176 / layout_400x300）
    ├── calendar/            # 农历日历页面
    └── stock/               # 股票页面（layout 单文件不分尺寸）
```

---

## 5. 核心模块详解

### 5.1 main.rs — 程序入口与硬件初始化

**文件路径：** `src/main.rs`

#### 5.1.1 程序入口

```rust
#![no_std]   // 不使用标准库
#![no_main]  // 不使用标准 main 入口

esp_bootloader_esp_idf::esp_app_desc!();  // 生成应用描述信息

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // ...
}
```

**关键变化（相比旧版）：**
- 入口宏从 `#[esp_hal_embassy::main]` 变为 `#[esp_rtos::main]`
- 返回类型从隐式变为显式 `-> !`（永不返回）
- 新增 `esp_bootloader_esp_idf::esp_app_desc!()` 宏生成 IDF 兼容的应用描述

#### 5.1.2 初始化流程

```
esp_alloc::heap_allocator!(size: 64 * 1024)  ← 初始化 64KB 全局堆
  │
hal_init(HalConfig::default().with_cpu_clock(CpuClock::max()))  ← HAL 初始化
  │
TimerGroup + SoftwareInterruptControl     ← 配置 Embassy 定时器
  │
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0)  ← 启动 RTOS 调度
  │
Rtc::new(peripherals.LPWR)                ← 初始化 RTC
  │
storage::enter_process()                  ← 从 Flash 加载配置
  │
GPIO 引脚分配                              ← 直接从 peripherals 获取
  │
SPI 初始化 (SpiConfig + Rate::from_mhz(32))  ← 创建 SPI 总线
  │
SPI 总线共享 (CsMutex + CriticalSectionDevice)  ← EPD/SD 共享
  │
spawner.spawn(display::render)            ← 启动显示渲染任务
  │
SdCard + VolumeManager                    ← 挂载 SD 卡
  │
ADC + 按键配置                             ← 配置 ADC 用于多按键检测
  │
spawner.spawn(event::run)                 ← 启动事件检测任务
  │
检查 WiFi 配置                             ← 判断 AP/STA 模式
  │
  ├── 未配置 → AP 模式配网
  └── 已配置 → STA 模式连接
       ├── spawn(battery::test_bat_adc)
       ├── spawn(worldtime::ntp_worker)
       └── spawn(pages::main_task)
```

#### 5.1.3 内存分配器（esp-alloc）

```rust
esp_alloc::heap_allocator!(size: 64 * 1024);  // 64KB 堆内存
```

**与旧版差异：**
- 旧版使用 `embedded-alloc` + 手动 `unsafe { ALLOCATOR.init(...) }` 初始化
- 新版使用 `esp-alloc` 宏一行搞定
- 堆大小现为 64KB（HTTP/TLS 大缓冲已移至 `.bss`，由 `WIFI_LOCK` 串行化独占，故不必占堆）
- `esp-alloc` 自动管理 ESP32-C3 的内存区域，更安全可靠

#### 5.1.4 HAL 初始化

```rust
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let peripherals = hal_init(config);
```

**与旧版差异：**
- 旧版：`Peripherals::take()` → `SystemControl::new()` → `ClockControl::max().freeze()` → 多步骤
- 新版：`hal_init(HalConfig)` 一行完成所有初始化，返回 `Peripherals`
- 时钟配置从 `ClockControl::max(system.clock_control).freeze()` 简化为 `CpuClock::max()`
- 不再需要手动管理系统时钟控制器

#### 5.1.5 GPIO 新 API

```rust
// 新版：直接从 peripherals 获取 GPIO
let epd_busy = peripherals.GPIO6;
let epd_cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());

// 旧版需要先创建 Io 对象
// let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
// let epd_busy = io.pins.gpio6;
```

esp-hal v1.x 中 GPIO 引脚直接从 `Peripherals` 获取，不再需要中间 `Io` 对象。`Output::new` 增加 `OutputConfig` 参数。

#### 5.1.6 SPI 新 API

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

**与旧版差异：**
- 旧版：`Spi::new(peripherals.SPI2, 32u32.MHz(), SpiMode::Mode0, &clocks)` 需传入时钟引用
- 新版：使用 `SpiConfig` 结构体配置，不再需要 `&clocks` 参数
- 频率使用 `Rate::from_mhz(32)` 代替 `32u32.MHz()`
- `Mode::_0` 使用 PascalCase 枚举变体（Rust 2024 edition 约定）

#### 5.1.7 SPI 总线共享

```rust
use critical_section::Mutex as CsMutex;

let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);

let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();
```

**与旧版差异：**
- 使用 `critical_section::Mutex` 替代 `embassy_sync` 的 `CriticalSectionRawMutex` + `RefCell`
- `CriticalSectionDevice::new_no_delay()` 替代旧的 `CriticalSectionDevice::new()`（不再需要 Delay 参数）
- `embedded-hal-bus` 从 0.1 升级到 0.3，API 有所变化

#### 5.1.8 Embassy RTOS 启动

```rust
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

**与旧版差异：**
- 旧版：`esp_hal_embassy::init(&clocks, systimer.alarm0)` 手动配置定时器
- 新版：`esp_rtos::start()` 统一管理，传入硬件定时器和软件中断
- 不再需要 `&clocks` 引用

### 5.2 display.rs — 显示渲染服务

**文件路径：** `src/display.rs`

#### 5.2.1 架构设计

显示系统采用 **生产者-消费者** 模式，页面绘制到帧缓冲区后通过 Channel 发送渲染请求。

```
┌──────────────┐    RENDER_CHANNEL    ┌──────────────┐    SPI    ┌─────────┐
│  页面 (Pages) │ ──── RenderInfo ───► │ render task  │ ────────► │ EPD 屏幕 │
└──────────────┘                       └──────────────┘           └─────────┘
```

#### 5.2.2 关键类型

```rust
// 条件编译选择屏幕尺寸
#[cfg(feature = "epd2in9")]
pub type EpdDisplay = Display2in9;          // 296×128

#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;          // 400×300

// SPI 设备类型（新版泛型参数更简洁）
type ActualSpi<'a> = CriticalSectionDevice<
    'a,
    Spi<'a, esp_hal::Blocking>,
    Output<'a>,
    embedded_hal_bus::spi::NoDelay,
>;
```

#### 5.2.3 RTC 内存属性变更

```rust
// 新版：unstable 前缀
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES: u32 = 0;

// 旧版：直接 rtc_fast
// #[ram(rtc_fast)]
```

esp-hal v1.x 中 `#[ram(...)]` 属性需要 `unstable()` 包装器。

#### 5.2.4 Unsafe 访问模式变更

```rust
// 新版：使用 core::ptr 显式操作
unsafe { core::ptr::addr_of_mut!(DISPLAY).write(Some(display)); }
unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut().unwrap().buffer() }

// 旧版：直接赋值
// unsafe { DISPLAY.replace(display); }
```

esp-hal v1.x 编译器对 `static mut` 访问更严格，改用 `core::ptr::addr_of_mut!` 避免未定义行为。

#### 5.2.5 GPIO 任务参数

```rust
#[embassy_executor::task]
pub async fn render(
    mut spi_device: &'static mut ActualSpi<'static>,
    busy: esp_hal::peripherals::GPIO6<'static>,   // 带生命周期
    rst: esp_hal::peripherals::GPIO7<'static>,
    dc: esp_hal::peripherals::GPIO20<'static>,
)
```

**与旧版差异：** GPIO 引脚现在是泛型参数化的 `esp_hal::peripherals::GPIOx<'static>`，而非 `Gpio6` 等类型别名。

#### 5.2.6 刷新策略

电子墨水屏有局部刷新（Quick）和全刷（Full）两种模式。每 N 次快刷后强制全刷一次清残影：N 的平台默认值是常量（`epd2in7` 为 20，其余为 5），但可在运行时被设置页 / Web 配置覆盖（`DisplayStorage.full_refresh_times`，`reload_full_refresh_times()` 钳制到 `1..=100`）：

```rust
// 非 epd2in7 路径
let need_clear = take_need_clear();   // 冷启动首帧为 true（rtc_fast）
let need_force_full = (get_render_times() % get_full_refresh_times() == 0 || need_clear)
    && refresh_lut == RefreshLut::Quick;
if need_force_full {
    spi_device = set_refresh_mode(RefreshLut::Full, &mut epd, spi_device);
}
```

2.7 寸屏（`epd2in7`）路径不同：用 `PREV_BUFFER`（5808B）保存上一帧做差分，唤醒/首帧用 `set_base_map_quiet` 静默过渡，常规帧走 `update_and_display_frame_partial`，周期性 `set_base_map` 全刷。`RENDER_TIMES`、`NEED_CLEAR`、`PREV_BUFFER` 均在 rtc_fast，深睡唤醒后保留。

### 5.3 event.rs — 事件系统

**文件路径：** `src/event.rs`

#### 5.3.1 事件类型

```rust
pub enum EventType {
    KeyShort(u32),       // 短按
    KeyLongStart(u32),   // 长按开始
    KeyLongIng(u32),     // 长按持续中（节流为每 100ms 派发一次，避免 ~1kHz 空转耗电）
    KeyLongEnd(u32),     // 长按释放
    KeyDouble(u32),      // 双击
    WheelBack,           // 滚轮后退（预留）
    WheelFront,          // 滚轮前进（预留）
}
```

#### 5.3.2 发布-订阅机制

```rust
struct Listener {
    callback: Box<dyn FnMut(EventInfo) -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + Sync>,
    event_type: EventType,
    ptr: Option<usize>,
    fixed: bool,
}

static LISTENER: Mutex<CriticalSectionRawMutex, Vec<Listener, 20>> = Mutex::new(Vec::new());
```

注册方式：
- `on(event_type, callback)` — 注册一次性事件监听
- `on_target(event_type, ptr, callback)` — 绑定到特定对象
- `on_fixed(event_type, ptr, callback)` — 常驻监听
- `clear()` — 清除非常驻监听器

#### 5.3.3 按键检测

```rust
#[embassy_executor::task]
pub async fn run(key1: esp_hal::peripherals::GPIO9<'static>, key2: esp_hal::peripherals::GPIO2<'static>) {
    let mut key1 = Input::new(key1, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
    let mut key2 = Input::new(key2, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
    // ...
}
```

**与旧版差异：** `Input::new` 使用 `InputConfig::default().with_pull(Pull::Up)` 替代 `Pull::Up` 直接传入。

#### 5.3.4 ADC 类型变更

```rust
// 新版：泛型参数使用具体的 peripherals 类型
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<
    AdcPin<esp_hal::peripherals::GPIO2<'static>, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>
>> = Mutex::new(None);

pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<
    Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>
>> = Mutex::new(None);
```

esp-hal v1.x 中 ADC 和 GPIO 的类型系统更加严格，需要指定具体的 peripherals 类型和 `Blocking` 模式。

### 5.4 wifi.rs — WiFi 与网络服务

**文件路径：** `src/wifi.rs`

**这是升级中变化最大的模块**，从 `esp-wifi` 完全迁移到 `esp-radio`。

#### 5.4.1 WiFi 初始化新 API

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

**与旧版差异：**

| 旧版 (esp-wifi 0.7) | 新版 (esp-radio 0.18) |
|---------------------|----------------------|
| `esp_wifi::initialize(EspWifiInitFor::Wifi, timer, rng, radio_clk, &clocks)` | 不需要手动初始化 |
| `esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)` | `esp_radio::wifi::new(wifi, config)` |
| 返回 `(interface, controller)` | 返回 `(controller, interfaces)` |
| `Rng::new(peripherals.RNG)` | `Rng::new()` 无需参数 |
| 需要 `TIMG0`、`RADIO_CLK`、`&clocks` 参数 | 只需 `wifi` 外设 |

#### 5.4.2 网络栈新 API

```rust
// 新版：embassy_net::new() 返回 (Stack, Runner)
let (stack, runner) = embassy_net::new(
    wifi_interface,
    config,
    make_static!(StackResources::<4>::new()),
    seed,
);
let stack: &Stack<'static> = &*make_static!(stack);

spawner.spawn(net_task(runner).unwrap());  // Runner 是独立任务
```

**与旧版差异：**

| 旧版 (embassy-net 0.4) | 新版 (embassy-net 0.9) |
|------------------------|----------------------|
| `Stack::new(interface, config, resources, seed)` | `embassy_net::new(interface, config, resources, seed)` |
| `Stack` 自身运行网络栈 | 返回 `(Stack, Runner)` 拆分 |
| `spawn(net_task(&stack))` | `spawn(net_task(runner))` |
| `Stack<WifiDevice<'static, WifiStaDevice>>` | `Stack<'static>`（简化泛型） |

#### 5.4.3 WiFi 事件监听新 API

```rust
// 新版：使用 subscribe + next_event_pure
loop {
    let mut subscriber = controller.subscribe().unwrap();
    let close_signal = STOP_WIFI_SIGNAL.wait();
    match select(subscriber.next_event_pure(), close_signal).await {
        Either::First(_) => {
            drop(subscriber);
            if !controller.is_connected() { /* 断开 */ }
        }
        Either::Second(_) => {
            drop(subscriber);
            let _ = controller.disconnect_async().await;
        }
    }
}
```

**与旧版差异：**
- 旧版：`controller.wait_for_event(WifiEvent::StaDisconnected).await`
- 新版：`controller.subscribe().unwrap()` + `subscriber.next_event_pure().await`
- 新增 `controller.is_connected()` 替代 `get_wifi_state()` 查询
- `disconnect_async()` 替代隐式异步 disconnect

#### 5.4.4 WiFi 生命周期

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

#### 5.4.5 AP 模式

```rust
let ap_config = WifiConfig::AccessPoint(
    AccessPointConfig::default()
        .with_ssid("esp_wifi")
        .with_password("123456789".into())
);

let (controller, interfaces) = esp_radio::wifi::new(wifi, ControllerConfig::default().with_initial_config(ap_config)).unwrap();
let wifi_ap_interface = interfaces.access_point;
```

AP 模式下 ESP32 自建 DHCP + DNS 服务：
- **DHCP**：监听 UDP 67，分配 IP `192.168.2.2`
- **DNS**：监听 UDP 53，劫持所有域名到 `192.168.2.1`

#### 5.4.6 UDP API 变更

```rust
// 新版 recv_from 返回 (usize, UdpMetadata)
match udp_socket.recv_from(&mut buf).await {
    Ok((n, src)) => { /* src 是 UdpMetadata 类型 */ }
}

// 旧版返回 (usize, IpEndpoint)
// Ok((n, src)) => { /* src 是 IpEndpoint 类型 */ }
```

### 5.5 storage.rs — 数据持久化

**文件路径：** `src/storage.rs`

#### 5.5.1 Flash 访问

```rust
pub fn write_flash(flash_addr: u32, bytes: &[u8]) -> Result<(), FlashStorageError> {
    let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
    let mut flash = FlashStorage::new(flash);
    flash.write(flash_addr, bytes)
}
```

**与旧版差异：**
- 旧版：`FlashStorage::new()` 每次创建新实例
- 新版：需要先 `FLASH::steal()` 获取外设，再传给 `FlashStorage::new(flash)`

#### 5.5.2 存储布局

```
NVS 分区 (0x9000 起始)
  ├── VersionStorage     偏移 +0x00     版本号 + 初始化标记 (0x1234_abce)
  ├── WifiStorage        偏移 +sizeof    WiFi SSID + 密码 + 配置完成标记
  ├── WeatherStorage     偏移 +sizeof    天气 token + 城市 + 同步时间 + 缓存数据
  ├── SleepStorage       偏移 +sizeof    阅读睡眠 / 天气睡眠时长
  ├── OtherStorage       偏移 +sizeof    复用：首字符存默认主页(1=天气/2=日历/3=股票)
  ├── HolidayStorage     偏移 +sizeof    节假日 token + 同步时间 + 缓存数据
  ├── ErrorLogStorage    偏移 +sizeof    错误计数 + 最后错误信息
  ├── StockStorage       偏移 +sizeof    最多 5 对 (代码,名称) + count + selected
  └── DisplayStorage     偏移 +sizeof    全刷间隔 (0=未配置，display 侧 clamp)

Storage 分区 (0x310000 起始)
  └── 待机壁纸图片       8 字节头 + 像素数据
```

`StockStorage` 与 `DisplayStorage` 是后加的，追加在末尾、不动既有偏移，因此无需改 `INIT_TAG`——旧固件留下的原始字节由使用方钳制。

#### 5.5.3 序列化与宏

通过 `unsafe` 指针转换实现零拷贝序列化，`impl_storage!` 宏自动为结构体实现读写方法。`HolidayStorage` 使用分块写入（每块 128 字节）避免栈溢出。

### 5.6 sleep.rs — 低功耗管理

**文件路径：** `src/sleep.rs`

#### 5.6.1 深度睡眠流程

```
to_sleep_tips()
  │
  判断是否超过空闲时间
  │
  force_stop_wifi()
  │
  show_sleep() 显示待机画面
  │
  关闭墨水屏/SD 卡电源
  │
  save_time_to_rtc() 保存时间戳
  │
  Rtc.sleep_deep() 进入深度睡眠
```

#### 5.6.2 关键变化

```rust
// GPIO 类型简化
pub static EINK_PWER_PIN: Mutex<CriticalSectionRawMutex, Option<Output<'static>>> = Mutex::new(None);
// 旧版: Option<Output<GpioPin<21>>>

// RTC 时间获取
pub async fn get_rtc_ms() -> u64 {
    RTC_MANGE.lock().await.as_mut().unwrap().current_time_us() / 1000
}
// 旧版: .get_time_ms()
```

### 5.7 worldtime.rs — NTP 时间同步

**文件路径：** `src/worldtime.rs`

#### 5.7.1 网络地址类型

```rust
use no_std_net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
```

**与旧版差异：** 从 `esp_wifi::wifi::ipv4` 迁移到独立的 `no-std-net` crate，不再依赖 WiFi 库的网络类型。

#### 5.7.2 NTP Socket 异步接口

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

**与旧版差异：**
- 旧版使用 `async fn`（不稳定特性）
- 新版使用 `impl Future` 返回类型（Edition 2024 稳定写法）
- UDP `recv_from` 返回的 `meta` 使用 `.endpoint` 获取地址

#### 5.7.3 时间戳安全性改进

```rust
// 新版：增加有效性检查
if ts > 1577836800 && ts < 4102444800 {  // 2020-2100 范围
    if let Ok(now) = OffsetDateTime::from_unix_timestamp(current_second as i64) {
        clock.set_time(now).await;
    }
}
```

旧版直接使用 `unwrap()`，新版增加了时间戳范围验证。

其它要点：
- NTP 服务器池 `["ntp.aliyun.com", "ntp.tuna.tsinghua.edu.cn", "ntp1.aliyun.com"]` 循环遍历，**DNS 失败即切下一台**，越界才返回错误（不再单点卡死）。
- `TimestampGen::timestamp_sec()` 必须返回 UNIX 秒（sntpc 内部再加 NTP 纪元差），误用微秒会让 roundtrip/offset 失真。
- `ntp_worker` 默认每小时对时一次，失败退避（避免每秒空转耗电），且对时成功后联动同步天气与节假日。
- `CLOCK_RESTORED_THIS_BOOT`（普通内存，每次开机复位）与 `CLOCK_SYNC_TIME_SECOND`（rtc_fast）区分：界面显示时间应基于前者，避免唤醒瞬间 Clock 实例仍是 `UNIX_EPOCH` 显示成 1970。

### 5.8 request.rs — HTTP 客户端

**文件路径：** `src/request.rs`

```rust
pub struct RequestClient {
    stack: &'static Stack<'static>,  // 简化的 Stack 类型
    rng: RngWrapper,
    // ...
}
```

**与旧版差异：**
- `Stack<WifiDevice<'static, WifiStaDevice>>` 简化为 `Stack<'static>`
- TLS 使用 `UnsecureProvider::new::<Aes128GcmSha256>(&mut self.rng)` 替代旧版 `TlsContext::new(&config, &mut self.rng)`
- `reqwless` v0.14 的 Response API 有所变化

### 5.9 battery.rs — 电池管理

**文件路径：** `src/battery.rs`

```rust
#[ram(unstable(rtc_fast))]
static mut LAST_BATTERY_PERCENT: u32 = 0;
```

每 60 秒采样一次（`voltage = adc × 2`，分压系数；电量用平方曲线映射）。ESP32-C3 的 `read_oneshot` 在冷启动 / 通道切换时经常返回 `Err`，因此采用**出错重试、超限放弃**的策略：最多重试 50 次，期间保留上一次电量；超限才跳过本轮，避免死循环 + 持锁。`LAST_BATTERY_PERCENT` 镜像到 rtc_fast，供 `sleep_renderer` 在渲染路径里同步读取（不必锁 Mutex）。

### 5.10 panic.rs — 自定义异常处理

**文件路径：** `src/panic.rs`

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

**与旧版差异：**
- 旧版：`esp_backtrace::arch::backtrace()` 返回 `[Option<u32>; 10]`
- 新版：`Backtrace::capture()` 返回结构化 `Backtrace`，通过 `.frames()` 迭代

### 5.11 其余模块

| 模块 | 说明 |
|------|------|
| `location.rs` | **新增**。基于 IP 的自动定位：请求 `ip-api.com`（明文 HTTP）反查城市与经纬度，产出 `"lat:lon"` 供心知天气查询。走标准 `use_wifi` 三段式 |
| `weather.rs` | 双数据源（`weather-openmeteo` feature 切换）；`request()` 统一入口；含节假日同步（`HolidayInfo`，最多重试 3 次、失败冷却 60s）；rtc_fast 跨深睡标志 |
| `epd2in9_txt.rs` | TXT 分页引擎，按 feature 给出 2.9/2.7/4.2 三套 `LINES_NUM/WIDTH/HEIGHT` |
| `sd_mount.rs` | embedded-sdmmc 封装；**手动实现 FAT32 LFN 目录项写入**以支持中文书名；按短名派生 `.idx/.log` |
| `flash_sleep.rs` | 待机壁纸 Flash 存储（魔数 `"SLEP"` + 崩溃安全写入）；BMP→1bit 转换（ITU-R BT.601 灰度） |
| `web_service.rs` | 配网 Web 服务（见 5.4.5）；表单支持 20 字段、按 Content-Length 跨分段读 body |
| `random.rs` | `esp_hal::rng::Rng` → `rand_core::RngCore + CryptoRng` 封装，供 TLS/二维码使用 |

---

## 6. 数据模型层

**目录路径：** `src/model/`

| 文件 | 功能 |
|------|------|
| `seniverse.rs` | 心知天气 API 数据结构，风向转换，蒲福风级 |
| `open_meteo.rs` | Open-Meteo API 数据转换（WMO → 心知天气格式） |
| `holiday.rs` | 节假日数据结构（含日期、名称、是否休息日） |
| `lunar.rs` | 农历/节气/星座算法（1900-2100 预计算数据，支持闰月） |
| `stock.rs` | 股票模型：`KLine`/`ChartMode`(6 模式环形)/`RealtimeQuote`，新浪 K 线与腾讯盘口解析、URL 构造、价格格式化 |

---

## 7. UI 组件层

**目录路径：** `src/widgets/`

所有组件基于 `embedded-graphics` 的 `Drawable` trait，可绘制到任意 `DrawTarget`。

| 组件 | 文件 | 功能 |
|------|------|------|
| IconGridWidget | `icon_grid_widget.rs` | 图标网格菜单（主页面） |
| ListWidget | `list_widget.rs` | 可滚动列表（书签、文件列表） |
| Calendar | `calendar.rs` | 日历网格（公历+农历+节气+节假日） |
| WeatherIcon | `weather_icon.rs` | 天气图标（约 20 种天气类型，32×32 BMP） |
| TempChart | `temp_chart.rs` | 温度趋势折线图（高/低温双线） |
| KLine | `kline.rs` | 股票蜡烛图/折线（涨跌空心/实心、窗口截断） |
| QrcodeWidget | `qrcode_widget.rs` | 二维码生成与显示 |
| Battery | `battery.rs` | 电池图标 + 百分比 |
| draw_icon | `draw_icon.rs` | 状态图标（wifi / 加载中 / 月亮） |
| ScrollBar | `scroll_bar.rs` | 滚动条（水平/垂直，被 ListWidget 使用） |

---

## 8. 页面系统

**目录路径：** `src/pages/`

### 8.1 Page Trait

```rust
pub trait Page {
    fn new() -> Self;
    async fn render(&mut self);
    async fn run(&mut self, spawner: Spawner);
    async fn bind_event(&mut self);
}
```

### 8.2 页面路由

`MainPage` 维护菜单列表和当前选中页面，通过 `PageEnum` 路由：

```
MainPage.run()
  ├── current_page == None → 显示图标网格菜单
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
        → 返回后 bind_event() 重新绑定主菜单事件
```

### 8.3 页面功能

| 页面 | 按键映射 | 主要功能 |
|------|---------|---------|
| **MainPage** | Key1/2 上下，Key3 确认 | 图标网格菜单导航；冷启动进默认主页、唤醒回上次页（rtc_fast） |
| **ReadPage** | Key1 上页，Key2 下页，Key3 菜单 | 电子书阅读、书签、跳页（长按加速）、旋转、重建索引 |
| **WeatherPage** | Key1 刷天气，Key2 刷节假日，Key3 退出 | 天气概览、5 日预报、温度曲线；60s 定时唤醒刷新 |
| **CalendarPage** | Key1/2 长按切月，Key3 回本月，短按刷新 | 日历、农历、节气、节假日（休/班） |
| **StockPage** | Key1/2 切图模式，Key3 返回，长按1/2 切股票，长按3 刷新 | 分时/日K/周K/月K/折线/盘口；实时模式 120s、其它 12h 唤醒刷新 |
| **ImagePage** | Key1/2 选择，Key3 查看/菜单 | BMP 图片浏览和壁纸设置（仅 4.2 寸） |
| **SettingPage** | Key1/2 调整，Key3 确认 | 睡眠时长、自动定位、web 配置、股票选择、默认主页、全刷次数、重置 |
| **DebugPage** | Key3 退出，Key1 长按清除 | 错误日志查看 |

---

## 9. 关键设计模式

### 9.1 SPI 总线共享

```
SPI2 主机, 32MHz, Mode 0
  │
  CsMutex<RefCell<Spi>>
  ├── CriticalSectionDevice(CS=GPIO3) → EPD     (new_no_delay)
  └── CriticalSectionDevice(CS=GPIO5) → SD Card  (new_no_delay)
```

每次 SPI 操作时自动关中断实现互斥，`new_no_delay` 替代旧版需要 `Delay` 参数的构造方式。

### 9.2 事件驱动架构

```
按键 GPIO 下降沿 → event::run() → key_detection() → toggle_event()
  → Listener[] 回调 → 修改页面状态 → render() → RENDER_CHANNEL → display::render()
```

### 9.3 全局静态状态

```rust
// 模式 1: Embassy Mutex + Option
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);

// 模式 2: core::ptr 安全访问 static mut
unsafe { core::ptr::addr_of_mut!(DISPLAY).write(Some(display)); }
unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut() }

// 模式 3: StaticCell + make_static!
let clocks = make_static!(Clocks, clocks_val);

// 模式 4: RTC 快速内存
#[ram(unstable(rtc_fast))]
static mut PAGE_INDEX: i32 = 1;
```

### 9.4 条件编译

```rust
#[cfg(feature = "epd2in9")]
const LINES_NUM: u32 = 7;

#[cfg(feature = "epd4in2")]
const LINES_NUM: u32 = 22;

#[cfg(not(feature = "weather-openmeteo"))]
{ Self::request_seniverse(&weather_storage).await }

#[cfg(feature = "weather-openmeteo")]
{ Self::request_open_meteo(&weather_storage).await }
```

### 9.5 Channel 解耦

| Channel | 生产者 | 消费者 | 数据 |
|---------|--------|--------|------|
| `RENDER_CHANNEL` | 各页面 | `display::render` | `RenderInfo` |
| `QUICKLY_LUT_CHANNEL` | 页面 | `display::render` | `bool` |
| `STOP_WIFI_SIGNAL` | sleep/do_stop | connection_wifi | `()` |
| `RECONNECT_WIFI_SIGNAL` | use_wifi | connection_wifi | `()` |
| `REINIT_WIFI_SIGNAL` | use_wifi | connect_wifi | `()` |

---

## 10. 数据流

### 10.1 按键事件流

```
物理按键 → GPIO 下降沿 → event::run() → key_detection()
  → (ADC 区分按键 2/3) → toggle_event() → Listener 回调
  → 修改页面状态 → render() → RENDER_CHANNEL → display::render() → EPD
```

### 10.2 网络请求流

```
ntp_worker / weather 触发 → use_wifi() (加锁) → 获取 Stack
  → RequestClient::new(stack) → send_request(url)
  → (HTTP: reqwless | HTTPS: embedded-tls) → 解析 JSON
  → 保存到 Flash → finish_wifi() (释放锁)
```

### 10.3 渲染流水线

```
页面 → display_mut() 获取帧缓冲区 → clear_buffer(White)
  → embedded-graphics 绑定绘制 (Text/Rectangle/Widget)
  → RENDER_CHANNEL.send() → display::render() 接收
  → EPD 唤醒 → 检查刷新计数 → Quick/Full 刷新 → EPD 睡眠
```

---

## 11. 内存管理

### 11.1 内存分区

```
ESP32-C3 内存 (~400KB SRAM)
  │
  ├── 全局堆 (64KB)          esp-alloc 宏自动管理
  │     esp_alloc::heap_allocator!(size: 64 * 1024)
  │     用于 Vec, String, Box 等动态分配
  │
  ├── Embassy 任务栈          由 executor 自动分配
  │
  ├── 静态变量 (.bss + .data)  全局 Mutex, Channel, Signal 等
  │
  └── RTC 快速内存             #[ram(unstable(rtc_fast))]
        睡眠期间保持供电的变量
```

### 11.2 heapless 集合

固定容量集合优先使用 `heapless`，避免堆碎片化：

```rust
heapless::String<32>     // 固定 32 字节字符串
heapless::Vec<T, 20>     // 固定 20 元素 Vec
```

---

## 12. 异步架构

### 12.1 Embassy 任务列表

| 任务 | 启动位置 | 职责 |
|------|---------|------|
| `display::render` | main.rs | EPD 渲染循环 |
| `event::run` | main.rs | 按键检测与事件分发 |
| `battery::test_bat_adc` | main.rs | 电池 ADC 采样 (60s 周期) |
| `worldtime::ntp_worker` | main.rs | NTP 同步 + 天气/节假日同步 |
| `pages::main_task` | main.rs | 主界面事件循环 |
| `connection_wifi` | wifi.rs | WiFi STA 连接状态管理 |
| `net_task` | wifi.rs | 网络协议栈 Runner |
| `do_stop` | wifi.rs | WiFi 自动关闭 (30s 超时) |
| `dhcp_service` | wifi.rs | AP 模式 DHCP 服务 |
| `dns_service` | wifi.rs | AP 模式 DNS 劫持 |
| `connection_wifi_ap` | wifi.rs | AP 模式连接管理 |

### 12.2 任务交互图

```
                    ┌─────────────┐
                    │   main.rs   │
                    └──────┬──────┘
                           │ spawn
           ┌───────────────┼───────────────┐
           ▼               ▼               ▼
    ┌────────────┐  ┌────────────┐  ┌────────────┐
    │ event::run │  │display::   │  │ntp_worker  │
    │            │  │render      │  │            │
    └─────┬──────┘  └──────┬─────┘  └─────┬──────┘
          │                │               │ use_wifi()
          │ toggle_event   │ Channel       ▼
          ▼                ▼        ┌────────────┐
    ┌────────────┐  ┌────────────┐  │connection_ │
    │pages::     │  │ EPD 硬件    │  │wifi        │
    │main_task   │  │            │  └──────┬─────┘
    └────────────┘  └────────────┘         │
                                           ▼
                                    ┌────────────┐
                                    │ net_task   │
                                    │ (Runner)   │
                                    └────────────┘
```

### 12.3 协作式调度

Embassy 使用协作式调度，任务通过 `.await` 主动让出执行权：

- `Timer::after(...).await` — 定时等待
- `channel.receive().await` — 等待消息
- `mutex.lock().await` — 等待锁
- `select(a, b).await` — 等待多个 Future 中的任意一个

---

## 13. 引脚分配表

| GPIO | 方向 | 功能 | 备注 |
|------|------|------|------|
| GPIO0 | 输出 | EPD MOSI (SPI) | SPI 数据输出 |
| GPIO1 | 输出 | SD 卡电源控制 | 低电平开启 |
| GPIO2 | 输入 | 按键 2 + ADC | 多按键 ADC 检测，支持 RTC 唤醒 |
| GPIO3 | 输出 | EPD CS (SPI 片选) | 高电平不选中 |
| GPIO4 | 输入 | 电池 ADC | 电池电压分压检测 |
| GPIO5 | 输出 | SD 卡 CS (SPI 片选) | 高电平不选中 |
| GPIO6 | 输入 | EPD BUSY | 屏幕忙状态 |
| GPIO7 | 输出 | EPD RST | 屏幕复位 |
| GPIO8 | 输出 | EPD SCLK (SPI 时钟) | SPI 时钟线 |
| GPIO9 | 输入 | 按键 1 | 上拉输入 |
| GPIO10 | 输入 | EPD MISO (SPI) | SPI 数据输入 |
| GPIO20 | 输出 | EPD DC (数据/命令) | SPI DC 信号 |
| GPIO21 | 输出 | 墨水屏电源控制 | 低电平开启 |

**SPI 配置：** SPI2 主机, 32MHz, Mode 0 (CPOL=0, CPHA=1)

---

## 14. 附录：esp-hal 升级要点

### 14.1 依赖迁移对照表

| 功能 | 旧版包名 | 新版包名 | 关键变化 |
|------|---------|---------|---------|
| HAL | esp-hal 0.19 | esp-hal 1.1 | API 全面重写 |
| WiFi | esp-wifi 0.7 | esp-radio 0.18 | 驱动分离，新初始化模式 |
| Embassy 集成 | esp-hal-embassy 0.2 | esp-rtos 0.3 | 统一运行时入口 |
| 内存分配 | embedded-alloc | esp-alloc 0.10 | 宏简化，自动管理 |
| 引导 | *(无)* | esp-bootloader-esp-idf 0.5 | IDF 兼容描述 |
| 网络 | embassy-net 0.4 | embassy-net 0.9 | Stack/Runner 分离 |
| 执行器 | embassy-executor 0.6 | embassy-executor 0.10 | 去除 task-arena-size |
| 同步 | embassy-sync 0.6 | embassy-sync 0.8 | API 微调 |
| 时间 | embassy-time 0.3 | embassy-time 0.5 | API 微调 |
| SPI 共享 | embedded-hal-bus 0.1 | embedded-hal-bus 0.3 | new_no_delay |

### 14.2 代码模式变更

| 模式 | 旧版写法 | 新版写法 |
|------|---------|---------|
| 外设获取 | `Peripherals::take()` | `hal_init(HalConfig)` |
| 时钟 | `ClockControl::max().freeze()` | `CpuClock::max()` |
| GPIO | `Io::new(peripherals.GPIO, peripherals.IO_MUX)` | `peripherals.GPIO6` |
| Output | `Output::new(pin, Level::High)` | `Output::new(pin, Level::High, OutputConfig::default())` |
| Input | `Input::new(pin, Pull::Up)` | `Input::new(pin, InputConfig::default().with_pull(Pull::Up))` |
| SPI | `Spi::new(spi2, freq, mode, &clocks)` | `Spi::new(spi2, SpiConfig::default().with_frequency(Rate::from_mhz(32)).with_mode(Mode::_0))` |
| SPI 共享 | `CriticalSectionDevice::new(mutex, cs, delay)` | `CriticalSectionDevice::new_no_delay(mutex, cs)` |
| RNG | `Rng::new(peripherals.RNG)` | `Rng::new()` |
| RTC 内存 | `#[ram(rtc_fast)]` | `#[ram(unstable(rtc_fast))]` |
| Static mut | `unsafe { DISPLAY.replace(x) }` | `unsafe { core::ptr::addr_of_mut!(DISPLAY).write(x) }` |
| WiFi init | `esp_wifi::initialize(...) + new_with_mode()` | `esp_radio::wifi::new(wifi, config)` |
| 网络 | `Stack::new(iface, config, res, seed)` | `embassy_net::new(iface, config, res, seed)` → `(Stack, Runner)` |
| WiFi 事件 | `controller.wait_for_event(Event)` | `controller.subscribe() + next_event_pure()` |
| Backtrace | `esp_backtrace::arch::backtrace()` | `Backtrace::capture().frames()` |
| Flash | `FlashStorage::new()` | `FlashStorage::new(FLASH::steal())` |
| 网络地址 | `esp_wifi::wifi::ipv4::*` | `no_std_net::*` |
| 异步 trait | `async fn send_to(...)` | `fn send_to(...) -> impl Future<...>` |
| 堆分配 | 手动 `embedded_alloc::Heap` + `init()` | `esp_alloc::heap_allocator!(size: 80*1024)` |
| 入口宏 | `#[esp_hal_embassy::main]` | `#[esp_rtos::main]` |
| 任务 spawn | `spawner.spawn(task(...)).ok()` | `spawner.spawn(task(...).unwrap())` |
| Stack 类型 | `Stack<WifiDevice<'static, WifiStaDevice>>` | `Stack<'static>` |

---

## 附录：构建与烧录

```bash
# 安装工具链
rustup target add riscv32imc-unknown-none-elf

# 安装 espflash
cargo install espflash

# 构建并烧录 4.2 寸版本（.cargo/config.toml 已配置 espflash 为 runner）
cargo run --release --features epd4in2

# 构建 2.7 寸版本
cargo run --release --features epd2in7

# 带调试模式（启动时若 Flash 有错误日志则进入调试页）
cargo run --release --features "epd4in2,enable_debug"
```

> 必须用 `--release`（`[profile.release] opt-level = "s"`），否则 `.text` 会溢出 3MB factory 分区。`epd2in9` 缺天气/日历页面布局，暂不可编译。
