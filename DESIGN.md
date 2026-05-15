# EPD Reader 详细设计文档

> 基于 Rust 的 ESP32-C3 电子墨水屏阅读器

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

---

## 1. 项目概述

### 1.1 项目简介

epd-reader 是一个运行在 **ESP32-C3** 上的嵌入式电子墨水屏（E-Paper Display）阅读器，使用 Rust 语言开发。项目实现了电子书阅读、天气查询、农历日历、图片浏览、WiFi 配网等功能，是一个功能完整的嵌入式应用。

### 1.2 功能列表

| 功能 | 说明 |
|------|------|
| 电子书阅读 | 支持 TXT 文件，自动分页、书签管理、进度显示 |
| 天气显示 | 支持心知天气 / OpenMeteo 双数据源，5 日预报 + 温度曲线 |
| 农历日历 | 公历/农历对照、节假日显示（含补班标记） |
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
| 显示屏 | Waveshare 2.9" 或 4.2" 电子墨水屏 |
| 存储 | MicroSD 卡（通过 SPI 连接） |
| Flash | 内置 4MB Flash |
| 输入 | 按键 ×3（其中一个通过 ADC 分压实现多按键） |
| 电源 | 锂电池 + ADC 电压检测 |

### 1.4 技术栈

| 类别 | 技术 | 用途 |
|------|------|------|
| 语言 | Rust (nightly-2024-09-19) | `#![no_std]` 嵌入式环境 |
| 目标平台 | `riscv32imc-unknown-none-elf` | ESP32-C3 RISC-V 核心 |
| HAL | esp-hal v0.19.0 | 硬件抽象层 |
| 异步运行时 | Embassy (executor + time) | 异步任务调度 |
| WiFi | esp-wifi v0.7.1 + embassy-net | 无线网络 + TCP/IP 协议栈 |
| 图形 | embedded-graphics v0.8 | 帧缓冲绘制框架 |
| 屏幕驱动 | epd-waveshare (自定义 fork) | 电子墨水屏底层驱动 |
| 字体 | u8g2-fonts | 中文字体渲染（GB2312） |
| 文件系统 | embedded-sdmmc v0.9.0 | SD 卡 FAT 文件系统 |
| 存储 | esp-storage | ESP32 内部 Flash 读写 |
| HTTP | reqwless + embedded-tls | HTTP/HTTPS 请求 |
| JSON | mini-json (自定义库) | 轻量 JSON 解析 |

---

## 2. 系统架构总览

### 2.1 整体架构图

```
┌─────────────────────────────────────────────────────────────────┐
│                        应用层 (Pages)                            │
│  MainPage ─┬─ ReadPage (电子书)   ─┬─ WeatherPage (天气)         │
│            ├─ CalendarPage (日历)   ├─ ImagePage (图片)          │
│            ├─ SettingPage (设置)    └─ DebugPage (调试)          │
├─────────────────────────────────────────────────────────────────┤
│                      UI 组件层 (Widgets)                         │
│  IconGridWidget │ ListWidget │ Calendar │ TempChart              │
│  WeatherIcon    │ QrcodeWidget│ Battery  │ ScrollBar             │
├─────────────────────────────────────────────────────────────────┤
│                      服务层 (Services)                           │
│  Display (渲染) │ Event (事件) │ WiFi (网络) │ Storage (持久化)   │
│  WorldTime (NTP)│ Weather (天气)│ Battery (电池)│ Sleep (睡眠)   │
│  Request (HTTP) │ WebService (配置)│ SDMount (文件) │ TxtReader   │
├─────────────────────────────────────────────────────────────────┤
│                      数据模型层 (Model)                          │
│  seniverse (天气数据) │ open_meteo (天气数据) │ lunar (农历)      │
│  holiday (节假日)                                       │
├─────────────────────────────────────────────────────────────────┤
│                      硬件抽象层 (HAL)                            │
│  esp-hal │ esp-wifi │ esp-storage │ embedded-hal │ embedded-graphics│
├─────────────────────────────────────────────────────────────────┤
│                      异步运行时 (Embassy)                        │
│  embassy-executor │ embassy-time │ embassy-sync │ embassy-net    │
├─────────────────────────────────────────────────────────────────┤
│                      硬件 (Hardware)                             │
│  ESP32-C3 │ E-Paper Display │ SD Card │ Buttons │ Battery       │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 模块依赖关系

```
main.rs
 ├── display.rs ◄── epd-waveshare, embedded-graphics
 ├── event.rs   ◄── embassy-sync, embassy-futures
 ├── pages/     ◄── event, display, widgets, model
 │   ├── main_page.rs      (页面管理器)
 │   ├── read_page.rs       ◄── epd2in9_txt, sd_mount
 │   ├── weather_page.rs    ◄── weather, worldtime
 │   ├── calendar_page.rs   ◄── weather, worldtime, widgets::calendar
 │   ├── image_page.rs      ◄── sd_mount, flash_sleep
 │   ├── setting_page.rs    ◄── wifi, web_service, storage
 │   └── debug_page.rs      ◄── storage
 ├── wifi.rs    ◄── esp-wifi, embassy-net, dhcparse
 ├── storage.rs ◄── esp-storage
 ├── weather.rs ◄── request, storage, worldtime
 ├── worldtime.rs ◄── sntpc, embassy-net
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
channel = "nightly-2024-09-19"
```

**说明：**
- ESP32-C3 使用 RISC-V 32 位指令集（`riscv32imc`），没有 MMU，不支持标准操作系统
- 需要 nightly Rust 以使用 `generic_const_exprs`、`type_alias_impl_trait` 等不稳定特性
- 固定版本号确保构建可复现

### 3.2 Cargo 配置 (`.cargo/config.toml`)

```toml
[target.riscv32imc-unknown-none-elf]
runner = "espflash flash --chip esp32c3 --baud 115200 --monitor --partition-table ./partitions.csv"

[build]
rustflags = [
  "-C", "linker-arg=-Tlinkall.x",
  "-C", "linker-arg=-Trom_functions.x",
  "-C", "force-frame-pointers",
]
target = "riscv32imc-unknown-none-elf"
```

**关键配置：**
- `runner`：`cargo run` 时自动调用 `espflash` 烧录并监控串口输出
- `-Tlinkall.x`：ESP32-C3 内存布局链接脚本（由 esp-hal 提供）
- `-Trom_functions.x`：ROM 函数链接脚本（使用 ESP32 内置 ROM 中的预编译函数）
- `force-frame-pointers`：保留帧指针，便于 backtrace 调试

### 3.3 Flash 分区表 (`partitions.csv`)

```csv
# Name,   Type, SubType,  Offset,   Size, Flags
nvs,      data, nvs,      ,         0x6000,    ← 非易失性存储 (24KB)
phy_init, data, phy,      ,         0x1000,    ← PHY 初始化数据 (4KB)
factory,  app,  factory,  ,         3M,        ← 应用程序 (3MB)
storage,  data, fat,      ,         128K,      ← FAT 文件系统 (128KB)
```

**说明：**
- `nvs` 分区（`0x9000` 起始）用于存储 WiFi 配置、天气数据、设置项等
- `factory` 分区存放编译后的固件
- `storage` 分区（`0x310000` 起始）用于存储待机壁纸图片

### 3.4 Feature Flags (`Cargo.toml`)

```toml
[features]
epd2in9 = []           # 2.9 寸屏幕 (296×128)
epd4in2 = []           # 4.2 寸屏幕 (400×300)
enable_debug = []      # 启用调试模式（启动时检查错误日志）
weather-openmeteo = [] # 使用 OpenMeteo 替代心知天气
```

**编译示例：**
```bash
cargo run --features epd4in2          # 构建 4.2 寸版本
cargo run --features epd2in9          # 构建 2.9 寸版本
cargo run --features "epd4in2,enable_debug"  # 带调试模式
```

### 3.5 核心依赖说明

```toml
# ── ESP 生态 ──
esp-hal = { version = "0.19.0", features = ["esp32c3", "async"] }  # 硬件抽象
esp-hal-embassy = { version = "0.2.0", features = ["esp32c3"] }    # Embassy 集成
esp-wifi = { version = "0.7.1", features = ["esp32c3", "async", "wifi", "embassy-net"] }
esp-storage = { version = "0.3.0", features = ["esp32c3"] }        # Flash 读写
esp-backtrace = { version = "0.13.0" }                              # 调用栈回溯
esp-println = { version = "0.10.0" }                                # 串口打印

# ── Embassy 异步框架 ──
embassy-executor = { version = "0.6.0", features = ["task-arena-size-98304"] }  # 98KB 任务 arena
embassy-time = { version = "0.3" }
embassy-net = { version = "0.4", features = ["dhcpv4", "tcp", "udp", "dns"] }
embassy-sync = { version = "0.6.0" }

# ── 图形与显示 ──
embedded-graphics = { version = "0.8" }    # 2D 图形绘制
epd-waveshare = { git = "..." }            # EPD 驱动 (自定义 fork)
u8g2-fonts = { version = "0.7.1" }         # 中文字体

# ── 存储 ──
embedded-sdmmc = { version = "0.9.0" }     # SD 卡文件系统

# ── 网络 ──
reqwless = { version = "0.11" }            # HTTP 客户端
embedded-tls = { version = "0.17" }         # TLS 实现
sntpc = { version = "0.3" }                # NTP 客户端
```

---

## 4. 模块结构

```
src/
├── main.rs              # 程序入口，硬件初始化，主循环
├── panic.rs             # 自定义 panic 处理器
├── display.rs           # 电子墨水屏渲染服务（独立 Embassy task）
├── event.rs             # 事件系统（按键检测 + 发布-订阅）
├── sleep.rs             # 深度睡眠与唤醒管理
├── wifi.rs              # WiFi STA/AP 模式，DHCP/DNS 服务
├── worldtime.rs         # NTP 时间同步，Clock 全局时钟
├── weather.rs           # 天气数据请求与缓存
├── request.rs           # HTTP/HTTPS 请求客户端
├── battery.rs           # 电池电压 ADC 采样
├── storage.rs           # Flash 持久化存储（NVS 区域）
├── sd_mount.rs          # SD 卡挂载与文件操作封装
├── epd2in9_txt.rs       # TXT 文本分页引擎
├── flash_sleep.rs       # 待机壁纸的 Flash 存储与渲染
├── web_service.rs       # WiFi 配网的 Web 服务
├── random.rs            # 随机数生成器封装
├── utils.rs             # 通用工具函数
│
├── model/               # 数据模型层
│   ├── mod.rs
│   ├── seniverse.rs     # 心知天气 API 数据结构
│   ├── open_meteo.rs    # Open-Meteo API 数据转换
│   ├── holiday.rs       # 节假日数据结构
│   └── lunar.rs         # 农历算法
│
├── widgets/             # UI 组件层
│   ├── mod.rs
│   ├── icon_grid_widget.rs  # 图标网格菜单
│   ├── list_widget.rs       # 可滚动列表
│   ├── weather_icon.rs      # 天气图标渲染
│   ├── temp_chart.rs        # 温度趋势图
│   ├── calendar.rs          # 日历组件（含农历）
│   ├── qrcode_widget.rs     # 二维码组件
│   ├── battery.rs           # 电池图标
│   └── scroll_bar.rs        # 滚动条
│
└── pages/               # 页面层
    ├── mod.rs               # Page trait 定义，main_task
    ├── main_page.rs         # 主菜单页面（页面管理器）
    ├── read_page.rs         # 电子书阅读页面
    ├── weather_page.rs      # 天气展示页面
    ├── calendar_page.rs     # 日历页面
    ├── image_page.rs        # 图片浏览页面
    ├── setting_page.rs      # 设置/配网页面
    ├── debug_page.rs        # 调试信息页面
    └── read_menu_page.rs    # 阅读菜单（占位）
```

---

## 5. 核心模块详解

### 5.1 main.rs — 程序入口与硬件初始化

**文件路径：** `src/main.rs`

#### 5.1.1 程序入口

```rust
#![no_std]   // 不使用标准库
#![no_main]  // 不使用标准 main 入口（由 esp-hal 提供 entry point）

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // ...
}
```

嵌入式环境没有操作系统支持，`#![no_std]` 禁用标准库，`#![no_main]` 禁用标准入口点。`#[esp_hal_embassy::main]` 宏将函数设置为 Embassy 异步入口。

#### 5.1.2 初始化流程

```
alloc()                          ← 初始化全局内存分配器 (38KB 堆)
  │
Peripherals::take()              ← 获取所有硬件外设的所有权
  │
SystemControl + ClockControl     ← 配置系统时钟为最大频率
  │
Rtc::new()                       ← 初始化 RTC 控制器
  │
SystemTimer + embassy::init()    ← 初始化 Embassy 定时器
  │
storage::enter_process()         ← 从 Flash 加载持久化配置
  │
Io::new()                        ← 初始化 GPIO
  │
GPIO 引脚分配                     ← 配置 EPD/SD/按键/电池引脚
  │
SPI 初始化 (32MHz)               ← 创建 SPI 主机总线
  │
SPI 总线共享                      ← CriticalSectionDevice 封装 EPD/SD CS
  │
spawner.spawn(display::render)   ← 启动显示渲染任务
  │
SdCard + VolumeManager           ← 挂载 SD 卡
  │
按键 ADC 配置                     ← 配置 ADC 用于多按键检测
  │
spawner.spawn(event::run)        ← 启动事件检测任务
  │
检查 WiFi 配置                    ← 判断是否需要 AP 配网
  │
  ├── 未配置 → AP 模式配网
  └── 已配置 → STA 模式连接
       │
       ├── spawn(battery::test_bat_adc)   ← 电池监测任务
       ├── spawn(worldtime::ntp_worker)   ← NTP 同步任务
       └── spawn(pages::main_task)        ← 主界面任务
```

#### 5.1.3 内存分配器

```rust
fn alloc() {
    const HEAP_SIZE: usize = 38 * 1024;     // 38KB 堆内存
    static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    static ALLOCATOR: embedded_alloc::Heap = embedded_alloc::Heap::empty();
    unsafe {
        ALLOCATOR.init(&mut HEAP as *const u8 as usize, core::mem::size_of_val(&HEAP))
    };
}
```

ESP32-C3 有约 400KB SRAM，分配 38KB 作为全局堆，支持 `alloc::vec::Vec`、`alloc::string::String` 等动态分配类型。

#### 5.1.4 SPI 总线共享

EPD 和 SD 卡共用 SPI2 总线，通过 `CriticalSectionDevice` + `RefCell<Mutex>` 实现独占访问：

```rust
let mut_spi = Mutex::new(RefCell::new(spi));
let mut_spi_static = make_static!(Mutex<RefCell<Spi<SPI2, FullDuplexMode>>, mut_spi);

let spi_bus_sd = CriticalSectionDevice::new(mut_spi_static, sdcard_cs, Delay);
let spi_bus_epd = CriticalSectionDevice::new(mut_spi_static, epd_cs, Delay);
```

`CriticalSectionDevice` 在每次 SPI 通信时自动加锁（关中断），通信完成后释放，确保总线互斥访问。

### 5.2 display.rs — 显示渲染服务

**文件路径：** `src/display.rs`

#### 5.2.1 架构设计

显示系统采用 **生产者-消费者** 模式，页面（生产者）绘制到帧缓冲区后发送渲染请求，渲染任务（消费者）将帧缓冲区数据发送到电子墨水屏。

```
┌──────────────┐    RENDER_CHANNEL    ┌──────────────┐    SPI    ┌─────────┐
│  页面 (Pages) │ ──── RenderInfo ───► │ render task  │ ────────► │ EPD 屏幕 │
│  绘制到 buffer │                      │ (消费者)     │           │         │
└──────────────┘                       └──────────────┘           └─────────┘
```

#### 5.2.2 关键数据结构

```rust
pub struct RenderInfo {
    pub time: i32,
    pub need_sleep: bool,  // 渲染完成后是否让屏幕进入睡眠
}

// 帧缓冲区（全局静态）
pub static mut DISPLAY: Option<EpdDisplay> = None;

// 渲染请求通道
pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex, RenderInfo, 64> = Channel::new();

// 刷新模式切换通道
pub static QUICKLY_LUT_CHANNEL: Channel<CriticalSectionRawMutex, bool, 64> = Channel::new();
```

#### 5.2.3 条件编译 — 屏幕适配

```rust
#[cfg(feature = "epd2in9")]
pub type EpdDisplay = Display2in9;          // 296×128 分辨率

#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;          // 400×300 分辨率
```

通过类型别名，上层代码使用 `EpdDisplay` 即可，编译时自动选择对应尺寸的实现。

#### 5.2.4 刷新策略

电子墨水屏有局部刷新（Quick）和全刷（Full）两种模式。局部刷新快但有残影，全刷清除残影但慢。系统采用混合策略：

```rust
const FORCE_FULL_REFRESH_TIMES: u32 = 5;  // 每 5 次局部刷新后强制全刷一次

if get_render_times() % FORCE_FULL_REFRESH_TIMES == 0 && refresh_lut == RefreshLut::Quick {
    // 切换到全刷模式
    spi_device = set_refresh_mode(RefreshLut::Full, &mut epd, spi_device);
}
epd.update_and_display_frame(&mut spi_device, buffer, &mut Delay);
if need_force_full {
    // 切回局部刷新模式
    spi_device = set_refresh_mode(RefreshLut::Quick, &mut epd, spi_device);
}
```

#### 5.2.5 屏幕睡眠

渲染完成后可让 EPD 进入睡眠以省电。下次渲染前自动唤醒：

```rust
if is_sleep {
    epd.wake_up(&mut spi_device, &mut Delay);
    is_sleep = false;
}
// ... 渲染 ...
if render_info.need_sleep {
    is_sleep = true;
    epd.sleep(&mut spi_device, &mut Delay);
}
```

### 5.3 event.rs — 事件系统

**文件路径：** `src/event.rs`

#### 5.3.1 事件类型

```rust
pub enum EventType {
    KeyShort(u32),       // 短按 (按键编号)
    KeyLongStart(u32),   // 长按开始
    KeyLongIng(u32),     // 长按持续中
    KeyLongEnd(u32),     // 长按释放
    KeyDouble(u32),      // 双击
    WheelBack,           // 滚轮后退
    WheelFront,          // 滚轮前进
}
```

#### 5.3.2 发布-订阅机制

```rust
struct Listener {
    callback: Box<dyn FnMut(EventInfo) -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + Sync>,
    event_type: EventType,
    ptr: Option<usize>,  // 目标对象裸指针
    fixed: bool,         // 是否常驻监听
}

static LISTENER: Mutex<CriticalSectionRawMutex, Vec<Listener, 20>> = Mutex::new(Vec::new());
```

**注册方式：**
- `on(event_type, callback)` — 注册一次性事件监听
- `on_target(event_type, ptr, callback)` — 绑定到特定对象
- `on_fixed(event_type, ptr, callback)` — 常驻监听，不被 `clear()` 清除
- `clear()` — 清除所有非常驻监听器

#### 5.3.3 按键检测逻辑

按键检测运行在独立的 Embassy task 中，使用 `select` 同时等待两个按键：

```rust
#[embassy_executor::task]
async fn run(mut key1: Gpio9, mut key2: Gpio2) {
    loop {
        let key1_edge = key1.wait_for_falling_edge();
        let key2_edge = key2.wait_for_falling_edge();
        match select(key1_edge, key2_edge).await {
            First(_) => key_detection::<_, 1>(&mut key1).await,
            Second(_) => key_detection::<_, 2>(&mut key2).await,
        }
        refresh_active_time().await;  // 刷新活跃时间，防止误睡
    }
}
```

**短按/长按/双击检测流程：**

```
检测到下降沿
  │
  开始计时
  │
  ├── 持续按下 < 500ms → 释放
  │     │
  │     ├── 等待 400ms 内是否有第二次按下
  │     │     ├── 有 → 触发 KeyDouble
  │     │     └── 超时 → 触发 KeyShort
  │
  └── 持续按下 ≥ 500ms
        │
        ├── 首次 → 触发 KeyLongStart
        └── 持续 → 触发 KeyLongIng
        │
        释放 → 触发 KeyLongEnd
```

#### 5.3.4 ADC 多按键检测

Key2 引脚同时连接多个按键，通过不同电阻分压产生不同 ADC 值来区分：

```rust
async fn judge_adc_num() -> usize {
    // 读取 20 次 ADC 取平均值
    let avg = adc_valute_sum / 20;
    // 根据电压值判断按键编号
    let temp = if avg < 200 { 2 } else { 3 };
    if avg > 1000 { return 0; }  // 无按键按下
    temp
}
```

### 5.4 wifi.rs — WiFi 与网络服务

**文件路径：** `src/wifi.rs`

#### 5.4.1 双模式支持

| 模式 | 用途 | 触发条件 |
|------|------|---------|
| **STA** (Station) | 连接路由器，访问互联网 | 已有 WiFi 配置 |
| **AP** (Access Point) | 创建热点，供手机连接配网 | 首次使用或无配置 |

#### 5.4.2 STA 模式流程

```
connect_wifi()
  │
  initialize(EspWifiInitFor::Wifi, ...)  ← 初始化 WiFi 驱动
  │
  esp_wifi::wifi::new_with_mode(WifiStaDevice)  ← 创建 STA 接口
  │
  Stack::new(wifi_interface, Config::dhcpv4(...))  ← 创建网络栈
  │
  spawn(connection_wifi)   ← 启动 WiFi 连接管理任务
  spawn(net_task)          ← 启动网络栈运行任务
  spawn(do_stop)           ← 启动 WiFi 自动关闭任务
  │
  等待 is_link_up + 获取 IP
  │
  保存 Stack 引用到全局变量
```

#### 5.4.3 WiFi 生命周期管理

```
                    ┌──────────────────┐
                    │  WifiStopped     │ ◄── force_stop_wifi()
                    └────────┬─────────┘
                             │ RECONNECT_WIFI_SIGNAL
                             ▼
  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
  │ WifiDisconnected│──►│WifiConnecting │──►│WifiConnected │
  └──────────────┘   └──────────────┘   └──────┬───────┘
         ▲                                       │
         │  StaDisconnected 事件                 │ STOP_WIFI_SIGNAL
         └───────────────────────────────────────┘
```

**WiFi 锁机制：** 使用 `WIFI_LOCK` 互斥锁确保同一时刻只有一个任务使用 WiFi：

```rust
pub async fn use_wifi() -> Result<&'static Stack<...>, WifiNetError> {
    // 等待锁释放
    // 检查 WiFi 状态，必要时初始化或重连
    // 加锁
    *WIFI_LOCK.lock().await = true;
    Ok(stack)
}

pub async fn finish_wifi() {
    // 释放锁
    *WIFI_LOCK.lock().await = false;
}
```

**自动关闭：** 30 秒无使用自动断开 WiFi 省电：

```rust
#[embassy_executor::task]
async fn do_stop() {
    loop {
        if Instant::now().as_secs() - LAST_USE_TIME_SECS > 30 {
            STOP_WIFI_SIGNAL.signal(());
        }
        Timer::after(Duration::from_millis(3000)).await;
    }
}
```

#### 5.4.4 AP 模式 — 自建 DHCP + DNS

AP 模式下 ESP32 自己实现 DHCP 和 DNS 服务，无需外部库：

- **DHCP 服务** (`dhcp_service` task)：监听 UDP 67 端口，响应 DISCOVER → OFFER、REQUEST → ACK，分配 IP `192.168.2.2`
- **DNS 服务** (`dns_service` task)：监听 UDP 53 端口，将所有域名解析到 `192.168.2.1`（DNS 劫持），使用户访问任意 URL 都能打开配置页面

### 5.5 storage.rs — 数据持久化

**文件路径：** `src/storage.rs`

#### 5.5.1 存储架构

所有持久化数据存储在 Flash 的 NVS 分区（`0x9000` 起始），通过 `esp-storage` 的 `FlashStorage` 直接读写。

**存储布局：**

```
NVS 分区 (0x9000 起始)
  │
  ├── VersionStorage     (偏移 +0x00)     版本号 + 初始化标记
  ├── WifiStorage        (偏移 +sizeof)    WiFi SSID + 密码
  ├── WeatherStorage     (偏移 +sizeof)    天气 API Key + 缓存数据
  ├── SleepStorage       (偏移 +sizeof)    睡眠超时配置
  ├── OtherStorage       (偏移 +sizeof)    保留
  ├── HolidayStorage     (偏移 +sizeof)    节假日缓存数据
  └── ErrorLogStorage    (偏移 +sizeof)    错误计数 + 最后错误信息

Storage 分区 (0x310000 起始)
  └── 待机壁纸图片       (8 字节头 + 像素数据)
```

#### 5.5.2 读写实现

通过 `unsafe` 的指针转换实现零拷贝序列化/反序列化：

```rust
fn serialize_storage<T>(storage: &T) -> [u8; size_of::<T>()] {
    unsafe { ptr::read(storage as *const _ as *const [u8; size_of::<T>()]) }
}

fn deserialize_storage<T>(data: &[u8]) -> T {
    unsafe { ptr::read(data.as_ptr() as *const T) }
}
```

#### 5.5.3 宏简化模板代码

```rust
macro_rules! impl_storage {
    ($type:ty, $offset:expr) => {
        impl NvsStorage for $type {
            fn read() -> Result<Self, FlashStorageError> {
                let mut buffer = [0u8; size_of::<Self>()];
                read_flash($offset as u32, &mut buffer)?;
                Ok(deserialize_storage(&buffer))
            }
            fn write(&self) -> Result<(), FlashStorageError> {
                let data = serialize_storage(self);
                write_flash($offset as u32, &data)
            }
        }
    };
}

// 使用
impl_storage!(VersionStorage, VERSION_STORAGE_OFFSET);
impl_storage!(WifiStorage, WIFI_STORAGE_OFFSET);
impl_storage!(WeatherStorage, WEATHER_STORAGE_OFFSET);
```

#### 5.5.4 分块写入

大结构体直接序列化会在栈上分配大数组，可能导致栈溢出。`HolidayStorage` 使用分块写入规避：

```rust
fn write_storage_chunked<T>(storage: &T, offset: u32) -> Result<(), FlashStorageError> {
    const CHUNK_SIZE: usize = 128;
    for chunk_offset in (0..total_size).step_by(CHUNK_SIZE) {
        // 每次只分配 128 字节栈空间
        let mut chunk = [0u8; CHUNK_SIZE];
        ptr::copy_nonoverlapping(...);
        write_flash(offset + chunk_offset as u32, &chunk[..chunk_size])?;
    }
    Ok(())
}
```

#### 5.5.5 存储初始化

首次启动时通过 `INIT_TAG`（魔数 `0x1234abcb`）判断是否需要初始化。修改存储结构体后需更新此值。

### 5.6 sleep.rs — 低功耗管理

**文件路径：** `src/sleep.rs`

#### 5.6.1 深度睡眠流程

```
to_sleep_tips()
  │
  判断是否超过空闲时间
  │
  force_stop_wifi()          ← 等待 WiFi 任务完成后断开
  │
  show_sleep()               ← 显示待机画面
  │
  EINK_PWER_PIN.set_high()   ← 关闭墨水屏电源
  SD_PWER_PIN.set_high()     ← 关闭 SD 卡电源
  │
  save_time_to_rtc()         ← 保存当前时间戳到 RTC 内存
  │
  Rtc.sleep_deep(...)        ← 进入深度睡眠
```

#### 5.6.2 唤醒源

| 唤醒源 | 配置 | 用途 |
|--------|------|------|
| RTC GPIO | `WakeupLevel::Low` | 按键唤醒 |
| 定时器 | `TimerWakeupSource` | 定时唤醒（同步天气等） |

#### 5.6.3 RTC 快速内存

使用 `#[ram(rtc_fast)]` 属性将关键变量放入 RTC 快速内存。深度睡眠期间 RTC 内存保持供电，唤醒后数据不丢失：

```rust
#[ram(rtc_fast)]
static mut WHEN_SLEEP_RTC_MS: u64 = 0;        // 睡眠时的 RTC 时间

#[ram(rtc_fast)]
pub static mut PAGE_INDEX: i32 = 1;            // 当前页面索引

#[ram(rtc_fast)]
pub static mut LAST_BATTERY_PERCENT: u32 = 0;  // 最后电量百分比

#[ram(rtc_fast)]
pub static mut CLOCK_SYNC_TIME_SECOND: u64 = 0; // NTP 同步时间
```

### 5.7 worldtime.rs — NTP 时间同步

**文件路径：** `src/worldtime.rs`

#### 5.7.1 Clock 实现

系统时钟基于 NTP 同步的 UTC 时间，加上本地运行偏移量计算：

```rust
pub(crate) struct Clock {
    sys_start: Mutex<CriticalSectionRawMutex, OffsetDateTime>,
}

impl Clock {
    async fn set_time(&self, now: OffsetDateTime) {
        let elapsed = Instant::now().as_millis();
        *sys_start = now.checked_sub(Duration::milliseconds(elapsed as i64));
    }

    async fn now(&self) -> OffsetDateTime {
        *sys_start + Duration::milliseconds(Instant::now().as_millis() as i64)
    }

    async fn local(&self) -> OffsetDateTime {
        self.now().await.to_offset(UtcOffset::from_hms(8, 0, 0).unwrap())  // UTC+8
    }
}
```

#### 5.7.2 NTP Worker 任务

```rust
#[embassy_executor::task]
async fn ntp_worker() {
    // 1. 如果 RTC 内存有睡眠前的时间戳，用 (时间戳 + 睡眠时长) 恢复
    // 2. 循环：每 1 小时同步一次 NTP
    // 3. NTP 同步成功后，触发天气和节假日同步
    loop {
        if 需要同步 {
            match ntp_request(stack, clock).await {
                Ok(_) => {
                    Weather::sync_weather().await;
                    HolidayInfo::sync_holiday().await;
                }
                Err(_) => { 重试逻辑 }
            }
        }
        Timer::after(Duration::from_secs(sleep_sec)).await;
    }
}
```

NTP 服务器使用阿里云和清华源：`ntp.aliyun.com`、`ntp.tuna.tsinghua.edu.cn`。

### 5.8 weather.rs — 天气数据服务

**文件路径：** `src/weather.rs`

支持两个数据源，通过 feature flag 切换：

| 数据源 | Feature | URL |
|--------|---------|-----|
| 心知天气 | 默认 | `api.seniverse.com` |
| Open-Meteo | `weather-openmeteo` | `api.open-meteo.com` |

**缓存策略：** 天气数据 5 小时刷新一次，节假日数据按年刷新。数据缓存在 Flash 中，断电不丢失。

### 5.9 request.rs — HTTP 客户端

**文件路径：** `src/request.rs`

#### 5.9.1 请求客户端

```rust
pub struct RequestClient {
    stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
    rng: RngWrapper,              // TLS 随机数生成器
    rx_buffer: Vec<u8>,           // TCP 接收缓冲区 (4KB)
    tx_buffer: Vec<u8>,           // TCP 发送缓冲区 (4KB)
    tls_rx_buffer: Vec<u8>,      // TLS 接收缓冲区 (4KB)
    tls_tx_buffer: Vec<u8>,      // TLS 发送缓冲区 (4KB)
}
```

同时支持 HTTP 和 HTTPS（TLS 1.3，`Aes128GcmSha256`），使用 `reqwless` 发送请求，`embedded-tls` 处理加密。

### 5.10 battery.rs — 电池管理

**文件路径：** `src/battery.rs`

#### 5.10.1 ADC 采样

```rust
#[embassy_executor::task]
async fn test_bat_adc() {
    const V_MAX: u32 = 4100;  // 满电电压 (mV)
    const V_MIN: u32 = 3100;  // 截止电压 (mV)

    loop {
        // 读取 ADC 值 → 换算电压 → 计算百分比
        let voltage_mv = adc_value as f32 * 2.0;  // 1:2 分压器
        let percent = (normalized_voltage² * 100);  // 非线性近似锂电池曲线

        // 每 60 秒采样一次
        Timer::after_secs(60).await;
    }
}
```

电量百分比使用二次曲线近似锂电池放电特性，低电量（<20%）时输出警告。

### 5.11 epd2in9_txt.rs — 文本分页引擎

**文件路径：** `src/epd2in9_txt.rs`

#### 5.11.1 核心功能

- **自动分页：** 根据屏幕尺寸、字体大小、中英文混合宽度计算每页内容
- **索引生成：** 为每本书生成 `.idx` 索引文件，记录每页的文件偏移量
- **书签管理：** 支持添加/删除/预览书签
- **进度显示：** 显示当前页/总页数，百分比进度条

#### 5.11.2 字符宽度计算

```
字符类型判断：
  ├── ASCII 字符 → 宽度 8px
  ├── 中文/全角字符 → 宽度 16px (ZH_WIDTH)
  └── UTF-8 尾字节 → 宽度 0 (属于上一个字符)
```

### 5.12 sd_mount.rs — SD 卡文件系统

**文件路径：** `src/sd_mount.rs`

封装 `embedded-sdmmc` 的 `VolumeManager`，提供：

- 文件列表读取（`books/`、`images/` 目录）
- 文件读写操作
- URL 解码（Web 配网时处理文件名）
- 索引文件派生（`.txt` → `.idx`）

### 5.13 flash_sleep.rs — Flash 图像存储

**文件路径：** `src/flash_sleep.rs`

#### 5.13.1 待机壁纸存储

待机壁纸存储在 Flash 的 `storage` 分区（`0x310000` 起始）：

```
┌────────────────────────────────┐
│ Header (8 bytes)               │
│   magic: 0x534C4550 (4 bytes) │ ← 验证标记
│   size: 像素数据长度 (4 bytes) │
├────────────────────────────────┤
│ Pixel Data                     │
│   300×400 像素, 1-bit 压缩     │ ← 每行 38 字节, 共 400 行
└────────────────────────────────┘
```

#### 5.13.2 BMP 转换

支持将 1-bit/24-bit/32-bit BMP 转换为 1-bit 单色像素数据：

- 24/32-bit 转 1-bit 使用 ITU-R BT.601 亮度公式：`(R*299 + G*587 + B*114) / 1000 < 128`
- 支持缩放到目标尺寸
- 提供流式逐行处理接口（避免大内存分配）

### 5.14 web_service.rs — Web 配置服务

**文件路径：** `src/web_service.rs`

运行在 TCP 80 端口的 HTTP 服务，用于 WiFi 配网和设备管理：

- 显示设备 IP 和二维码
- 处理 WiFi 配置提交
- 提供文件列表和文件上传
- 支持设备重置

### 5.15 panic.rs — 自定义异常处理

**文件路径：** `src/panic.rs`

```rust
#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    // 1. 构建 200 字符以内的错误信息（位置 + 消息 + 调用栈）
    // 2. 保存到 ErrorLogStorage (Flash)
    // 3. 打印到串口
    // 4. 死循环（等待看门狗重启）
    loop {}
}
```

配合 `enable_debug` feature，启动时检查是否有错误日志，有则自动进入调试页面。

---

## 6. 数据模型层

**目录路径：** `src/model/`

### 6.1 seniverse.rs — 心知天气数据模型

```rust
pub struct DailyResult {
    pub location: Location,         // 地理位置
    pub daily: Vec<Daily, 5>,      // 5 日天气数据
}

pub struct Daily {
    pub date: String<16>,           // 日期
    pub text_day: String<32>,       // 白天天气
    pub text_night: String<32>,     // 夜间天气
    pub high: String<8>,            // 最高温度
    pub low: String<8>,             // 最低温度
    pub humidity: String<8>,        // 湿度
    pub wind_direction: String<16>, // 风向
    pub wind_scale: String<8>,      // 风力等级
    pub rainfall: String<8>,        // 降水量
}
```

提供风向角度转中文、天气代码转换、蒲福风级等工具函数。

### 6.2 open_meteo.rs — Open-Meteo 数据转换

将 Open-Meteo API 的 WMO 天气代码转换为心知天气兼容的格式，统一上层展示逻辑。

### 6.3 holiday.rs — 节假日数据

```rust
pub struct Holiday {
    pub date: String<8>,     // MMdd 格式
    pub name: String<32>,    // 节日名称
    pub is_off_day: bool,   // 是否休息日
}

pub struct HolidayResponse {
    pub year: u32,
    pub holidays: Vec<Holiday, 400>,  // 最多 400 天数据
}
```

日历组件使用 `is_off_day` 在日期旁显示"休"或"班"标记。

### 6.4 lunar.rs — 农历算法

基于 1900-2100 年预计算数据（`LUNAR_DATA` 数组）的农历转换：

- 阳历 → 阴历日期
- 闰月处理
- 传统中文命名（正月、初一、十五等）

```rust
let lunar = Lunar::new(2024, 8);
if let Some(day) = lunar.get_lunar_day(15) {
    // day.get_month_name() → "八月"
    // day.get_day_name()   → "十五"
}
```

---

## 7. UI 组件层

**目录路径：** `src/widgets/`

所有组件基于 `embedded-graphics` 的 `Drawable` trait 实现，可绘制到任意 `DrawTarget`。

### 7.1 组件概览

| 组件 | 文件 | 功能 |
|------|------|------|
| IconGridWidget | `icon_grid_widget.rs` | 图标网格菜单（主页面） |
| ListWidget | `list_widget.rs` | 可滚动列表（书签、文件列表） |
| Calendar | `calendar.rs` | 日历网格（公历+农历+节假日） |
| WeatherIcon | `weather_icon.rs` | 天气图标（31 种天气类型） |
| TempChart | `temp_chart.rs` | 温度趋势折线图 |
| QrcodeWidget | `qrcode_widget.rs` | 二维码生成与显示 |
| Battery | `battery.rs` | 电池图标 + 百分比 |
| ScrollBar | `scroll_bar.rs` | 滚动条（水平/垂直） |

### 7.2 IconGridWidget — 图标网格

```
┌─────────┬─────────┬─────────┐
│  📖     │  ☀️     │  📅     │
│  电子书  │  天气    │  日历    │
├─────────┬─────────┼─────────┤
│  🖼️     │  ⚙️     │  🔧     │
│  图片    │  设置    │  调试    │
└─────────┴─────────┴─────────┘
```

- 可配置列数
- 选中项显示圆角高亮边框
- 图标使用 `embedded-graphics` 基础图形绘制（非位图）

### 7.3 Calendar — 日历组件

```
        2024年8月
日  一  二  三  四  五  六
              1   2   3
 4   5   6   7   8   9  10
11  12  13  14  15  16  17
18  19  20  21  22  23  24
25  26  27  28  29  30  31
```

每个日期下方显示农历（初一显示月名），节假日显示"休"/"班"标记。

### 7.4 TempChart — 温度趋势图

- 自动计算温度范围
- 每 5°C 一条水平参考线
- 高温/低温两条折线
- 数据点处标注温度值

### 7.5 WeatherIcon — 天气图标

31 种天气类型，使用 32×32 BMP 位图渲染。图标文件存放在 SD 卡 `icons/` 目录。

---

## 8. 页面系统

**目录路径：** `src/pages/`

### 8.1 Page Trait

所有页面实现统一的 `Page` trait：

```rust
pub trait Page {
    fn new() -> Self;
    async fn render(&mut self);         // 渲染页面内容
    async fn run(&mut self, spawner: Spawner);  // 主事件循环
    async fn bind_event(&mut self);     // 注册事件监听
}
```

### 8.2 页面路由

`MainPage` 作为页面管理器，维护菜单列表和当前选中页面：

```rust
enum PageEnum {
    EMainPage,
    EReadPage,
    EWeatherPage,
    ECalendarPage,
    EImageListPage,
    ESettingPage,
    EDebugPage,
}
```

页面切换流程：

```
MainPage.run() 主循环
  │
  ├── current_page == None → 显示菜单网格
  │
  └── current_page == Some(index)
        │
        match menus[index].page_enum {
            EReadPage → ReadPage::new().bind_event().run()
            EWeatherPage → WeatherPage::new().bind_event().run()
            ...
        }
        │
        run() 返回 → bind_event() 重新绑定主菜单事件
```

每次进入子页面时调用 `bind_event()` 注册该页面的事件处理器，退出时 `clear()` 清除，然后重新绑定主菜单事件。这种模式避免了事件冲突。

### 8.3 页面详情

| 页面 | 按键映射 | 主要功能 |
|------|---------|---------|
| **MainPage** | Key1/2 上下移动，Key3 确认 | 图标网格菜单导航 |
| **ReadPage** | Key1 上一页，Key2 下一页，Key3 菜单 | 电子书阅读、书签、跳页 |
| **WeatherPage** | 自动定时刷新 | 天气概览、7 日预报、温度曲线 |
| **CalendarPage** | Key1/2 长按切月，短按刷新 | 日历、农历、节假日 |
| **ImagePage** | Key1/2 选择图片，Key3 查看/菜单 | BMP 图片浏览和壁纸设置 |
| **SettingPage** | Key1/2 调整，Key3 确认 | WiFi 配网、睡眠时间、重置 |
| **DebugPage** | Key3 退出，Key1 长按清除 | 错误日志查看 |

---

## 9. 关键设计模式

### 9.1 SPI 总线共享

EPD 和 SD 卡共享 SPI2，通过 `CriticalSectionDevice` 实现：

```
SPI2 总线
  │
  ├── CriticalSectionDevice(CS=GPIO3) → EPD
  │     每次 SPI 操作前关中断、拉低 CS
  │     操作完成后恢复 CS、开中断
  │
  └── CriticalSectionDevice(CS=GPIO5) → SD Card
        同上
```

### 9.2 事件驱动架构

```
┌──────────┐   wait_for_falling_edge   ┌──────────────┐
│ 按键 GPIO │ ────────────────────────► │ event::run() │
└──────────┘                           │ (Embassy task)│
                                       └───────┬──────┘
                                               │ toggle_event()
                                               ▼
                                       ┌──────────────┐
                                       │  Listener[]  │
                                       │ (回调函数列表)│
                                       └───────┬──────┘
                                               │ 调用 callback
                                               ▼
                                       ┌──────────────┐
                                       │  当前 Page    │
                                       │ (修改状态)    │
                                       └───────┬──────┘
                                               │ 状态变化触发 render()
                                               ▼
                                       ┌──────────────┐
                                       │ RENDER_CHANNEL│
                                       └───────┬──────┘
                                               │
                                               ▼
                                       ┌──────────────┐
                                       │ display::render│
                                       │ (刷新屏幕)    │
                                       └──────────────┘
```

### 9.3 全局静态状态

嵌入式环境没有 `OnceCell`/`LazyLock` 等便利工具，项目使用以下模式管理全局状态：

```rust
// 模式 1: Embassy Mutex + Option
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);
// 初始化: WIFI_INFO.lock().await.replace(wifi);
// 使用: WIFI_INFO.lock().await.as_ref().unwrap()

// 模式 2: unsafe static mut
pub static mut DISPLAY: Option<EpdDisplay> = None;
// 读取: unsafe { DISPLAY.as_mut() }

// 模式 3: StaticCell (make_static! 宏)
let clocks = make_static!(Clocks, clocks_val);
// clocks: &'static Clocks

// 模式 4: #[ram(rtc_fast)] 深度睡眠保持
#[ram(rtc_fast)]
pub static mut PAGE_INDEX: i32 = 1;
```

### 9.4 条件编译

通过 feature flag 实现一套代码适配不同硬件配置：

```rust
// 屏幕尺寸适配
#[cfg(feature = "epd2in9")]
const LINES_NUM: u32 = 7;    // 2.9 寸屏 7 行文字
#[cfg(feature = "epd4in2")]
const LINES_NUM: u32 = 22;   // 4.2 寸屏 22 行文字

// 天气数据源适配
#[cfg(not(feature = "weather-openmeteo"))]
{ Self::request_seniverse(&weather_storage).await }

#[cfg(feature = "weather-openmeteo")]
{ Self::request_open_meteo(&weather_storage).await }
```

### 9.5 生产者-消费者 (Channel)

多个模块间通过 Embassy Channel 解耦：

| Channel | 生产者 | 消费者 | 数据 |
|---------|--------|--------|------|
| `RENDER_CHANNEL` | 各页面 | `display::render` | `RenderInfo` |
| `QUICKLY_LUT_CHANNEL` | 页面 | `display::render` | `bool` (刷新模式) |
| `STOP_WIFI_SIGNAL` | sleep/do_stop | connection_wifi | `()` |
| `RECONNECT_WIFI_SIGNAL` | use_wifi | connection_wifi | `()` |
| `REINIT_WIFI_SIGNAL` | use_wifi | connect_wifi | `()` |

---

## 10. 数据流

### 10.1 按键事件流

```
物理按键按下
  │
  GPIO 下降沿中断
  │
  event::run() 检测到按键
  │
  key_detection() 判断短按/长按/双击
  │ (Key2 通过 ADC 区分按键 2 或 3)
  │
  toggle_event(event_type)
  │
  遍历 Listener 列表，匹配 event_type
  │
  调用注册的回调函数 (async)
  │
  回调中修改页面状态 (need_render = true)
  │
  页面 run() 循环检测到状态变化
  │
  render() 绘制到帧缓冲区
  │
  RENDER_CHANNEL.send() 通知渲染任务
  │
  display::render() 将帧缓冲区发送到 EPD
```

### 10.2 网络请求流

```
ntp_worker / weather / holiday 触发请求
  │
  use_wifi() 获取 WiFi Stack（加锁）
  │
  ├── 如果 WiFi 未初始化 → REINIT_WIFI_SIGNAL → connect_wifi()
  ├── 如果 WiFi 已停止 → RECONNECT_WIFI_SIGNAL → 重连
  └── 如果 WiFi 已连接 → 直接使用
  │
  RequestClient::new(stack)
  │
  send_request(url)
  │
  ├── HTTP → reqwless 直接请求
  └── HTTPS → embedded-tls TLS 握手 → reqwless 请求
  │
  解析 JSON 响应 (mini-json)
  │
  保存到 Flash (NvsStorage::write())
  │
  finish_wifi()（释放锁）
```

### 10.3 渲染流水线

```
页面代码
  │
  display_mut() 获取帧缓冲区引用
  │
  clear_buffer(White)
  │
  使用 embedded-graphics 绑定绘制:
  ├── Text / TextBox (文字)
  ├── Rectangle / Line (几何图形)
  ├── Widget.draw(display) (自定义组件)
  └── FontRenderer.render_aligned() (中文字体)
  │
  RENDER_CHANNEL.send(RenderInfo { need_sleep })
  │
  display::render() 任务接收到请求
  │
  唤醒 EPD（如需）
  │
  检查刷新计数 → 决定 Quick/Full 刷新
  │
  epd.update_and_display_frame(buffer)
  │
  EPD 睡眠（如需）
```

---

## 11. 内存管理

### 11.1 内存分区

```
ESP32-C3 内存 (~400KB SRAM)
  │
  ├── Embassy task arena (98KB)
  │     embassy-executor feature = "task-arena-size-98304"
  │
  ├── 全局堆 (38KB)
  │     embedded-alloc::Heap, HEAP_SIZE = 38*1024
  │     用于 Vec<u8>, String, Box<dyn trait> 等动态分配
  │
  ├── 栈 (默认大小, 编译器分配)
  │     注意：嵌入式环境栈空间有限
  │     大数组使用堆或分块处理
  │
  ├── 静态变量 (.bss + .data)
  │     全局 Mutex, Channel, Signal 等
  │
  └── RTC 快速内存 (~8KB)
        #[ram(rtc_fast)] 属性的变量
        深度睡眠期间保持供电
```

### 11.2 heapless 集合

在 `#![no_std]` 环境下，优先使用 `heapless` 的固定容量集合：

```rust
heapless::String<32>     // 固定 32 字节字符串
heapless::Vec<T, 20>     // 固定 20 个元素的 Vec
```

这些集合不使用全局堆，直接在栈或静态存储中分配，避免了碎片化问题。

### 11.3 static_cell — 静态生命周期

`make_static!` 宏将值提升为 `'static` 生命周期，是 Embassy 任务间共享数据的标准模式：

```rust
let clocks = make_static!(Clocks, clocks_val);
// clocks 类型: &'static Clocks
// 可以安全地跨任务共享
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
| `connection_wifi` | wifi.rs | WiFi 连接状态管理 |
| `net_task` | wifi.rs | 网络协议栈运行 |
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
          │                │               │
          │ toggle_event   │ Channel       │ use_wifi()
          ▼                ▼               ▼
    ┌────────────┐  ┌────────────┐  ┌────────────┐
    │pages::     │  │ EPD 硬件    │  │connection_ │
    │main_task   │  │            │  │wifi        │
    └────────────┘  └────────────┘  └──────┬─────┘
                                          │
                                          ▼
                                   ┌────────────┐
                                   │ net_task   │
                                   │ (TCP/IP)   │
                                   └────────────┘
```

### 12.3 Embassy 仲裁器

Embassy 使用协作式调度（非抢占式），任务通过 `.await` 主动让出执行权：

- `Timer::after(...).await` — 定时等待
- `channel.receive().await` — 等待消息
- `mutex.lock().await` — 等待锁
- `select(a, b).await` — 等待多个 Future 中的任意一个完成

---

## 13. 引脚分配表

### 13.1 ESP32-C3 GPIO 映射

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

### 13.2 SPI 配置

```
SPI2 主机模式, 32MHz, Mode 0 (CPOL=0, CPHA=1)
  SCK  → GPIO8
  MOSI → GPIO0
  MISO → GPIO10
  CS0  → GPIO3  (EPD)
  CS1  → GPIO5  (SD Card)
```

---

## 附录：构建与烧录

```bash
# 安装工具链
rustup target add riscv32imc-unknown-none-elf

# 安装 espflash
cargo install espflash

# 构建 2.9 寸版本
cargo build --features epd2in9

# 构建并烧录
cargo run --features epd2in9

# 构建 4.2 寸版本
cargo run --features epd4in2

# 带调试模式
cargo run --features "epd4in2,enable_debug"
```
