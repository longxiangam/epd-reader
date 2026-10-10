# EPD Reader 详细设计文档

> English | [DESIGN_EN.md](DESIGN_EN.md)

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

epd-reader 是一个运行在 **ESP32-C3** 上的嵌入式电子墨水屏（E-Paper Display）阅读器，使用 Rust 语言开发。项目实现了电子书阅读、天气查询、农历日历、股票行情、图片浏览、WiFi 配网等功能，是一个功能完整的嵌入式应用。

本项目基于 **esp-hal v1.x** 生态构建，采用最新的 `esp-radio` 无线驱动、`esp-rtos` 异步运行时集成、`esp-alloc` 内存管理器，代表了 2025 年 Rust 嵌入式 ESP32 开发的最新实践。

硬件采用**板级抽象**：feature 双维度选择——`epd_*` 选屏幕驱动（epd2in9 / epd2in7 / epd4in2），`board_*` 选板硬件（board_v1 旧板 / board_v2 v22 新板），一个设备 = epd + board 组合，编译期固定全部 IO 分配。

### 1.2 功能列表

| 功能 | 说明 |
|------|------|
| 电子书阅读 | TXT 文件，TTF 矢量字体动态分页 + 内置点阵字体，书签管理、书单二级目录、排版设置（字号/行距/笔画加粗/旋转/方向键互换）、进度显示 |
| 天气显示 | 心知天气 / Open-Meteo 双数据源，5 日预报 + 温度曲线 + 室内温湿度（v2 板载 AHT20） |
| 自动定位 | 基于 IP 反查城市与经纬度，自动设置天气查询地点 |
| 农历日历 | 公历/农历对照、节气、节假日显示（含补班标记） |
| 股票行情 | 沪深股票总览 + 分时 / 日K / 周K / 月K / 折线 + 实时盘口，多股切换；开盘时段 120s 唤醒刷新，非开盘（午休/盘后/周末/节假日）睡到下一开盘时点 |
| 图片浏览 | 支持黑白 BMP 图片查看，可设为待机壁纸 |
| WiFi 配网 | AP 模式热点 + Web 配置界面，手机扫码即可配网；支持书籍/图片/壁纸的上传与管理 |
| NTP 时间 | 自动同步网络时间，对时结果写入硬件 RTC；唤醒优先从硬件 RTC 恢复 |
| 硬件 RTC | PCF8563（v2 板载，CR2032 备电，UTC 走时）：系统时钟源 + 深睡定时唤醒（秒/分档倒计时、超长间隔绝对时间报警，INT → GPIO4） |
| 电池管理 | ADC 采样电池电压，电量百分比显示 |
| 低功耗 | 自动深度睡眠，按键/定时器唤醒，外设电源独立控制 |
| 错误日志 | 自定义 Panic Handler，崩溃信息写入 Flash，重启后可查看 |

### 1.3 硬件平台

| 组件 | 型号/规格 |
|------|-----------|
| MCU | ESP32-C3 (RISC-V, 160MHz, 400KB SRAM) |
| 显示屏 | Waveshare 4.2" (400×300) / 2.7" (264×176) / 2.9" (296×128) 电子墨水屏（2.9" 天气/日历页为横屏适配，排版未精细调整） |
| 存储 | MicroSD 卡（通过 SPI 连接，≤32GB SD/SDHC，MBR 分区） |
| Flash | 内置 4MB Flash |
| 输入 | 按键 ×3：board_v1 旧板 = GPIO9 直连键（键1）+ GPIO2 ADC 分压梯（键2/3）；board_v2 新板 = 三键全走 GPIO4 ADC 分压梯 |
| I2C 外设 | 仅 board_v2：PCF8563 硬件 RTC（0x51）+ AHT20 温湿度（0x38），GPIO9=SDA / GPIO21=SCL |
| 电源 | 锂电池 + ADC 电压检测；v1 双电源轨（EINK=GPIO21 / SD=GPIO1，高电平断电），v2 单轨（GPIO1，低电平开启） |

### 1.4 技术栈

| 类别 | 技术 | 用途 |
|------|------|------|
| 语言 | Rust (Edition 2024, nightly) | `#![no_std]` 嵌入式环境 |
| 目标平台 | `riscv32imc-unknown-none-elf` | ESP32-C3 RISC-V 核心 |
| HAL | esp-hal v1.1.1 | 硬件抽象层 |
| RTOS 集成 | esp-rtos v0.3.0 | Embassy 异步运行时集成 |
| WiFi | esp-radio v0.18.0 | 无线网络驱动 |
| 网络栈 | embassy-net v0.9.0 | TCP/IP 协议栈 |
| 异步运行时 | embassy-executor v0.10.0 | 异步任务调度 |
| 内存 | esp-alloc v0.10.0 | 全局堆分配器 |
| 图形 | embedded-graphics v0.8 | 帧缓冲绘制框架 |
| 屏幕驱动 | epd-waveshare (自定义 fork) | 电子墨水屏底层驱动 |
| 字体 | u8g2-fonts v0.7.1 + ab_glyph_rasterizer v0.1 | 点阵中文（GB2312）+ TTF 矢量栅格化 |
| 文件系统 | embedded-sdmmc (fork `lfn-create` 分支) | SD 卡 FAT 文件系统（支持中文文件名创建） |
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
│  Location (IP 定位) │ TtfSd (矢量字)                             │
├─────────────────────────────────────────────────────────────────┤
│                 板级抽象 (Board: v1 旧板 / v2 新板)               │
│  电源轨 │ 按键梯/电池引脚 │ I2C (v2) │ PCF8563 (RTC) │ AHT20     │
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
│  PCF8563 RTC + AHT20 (I2C, 仅 v2)                               │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 模块依赖关系

```
main.rs
 ├── board/     ◄── esp-hal (GPIO/I2C/RTCIO)        ← 板级抽象（引脚/电源轨/I2C 按板型编译期固定）
 ├── display.rs ◄── epd-waveshare, embedded-graphics, critical-section
 ├── event.rs   ◄── embassy-sync, embassy-futures, esp-hal (GPIO/ADC), board
 ├── pages/     ◄── event, display, widgets, model, board
 │   ├── main_page.rs      (页面管理器/默认主页/唤醒回上次页)
 │   ├── read/             ◄── epd2in9_txt, ttf_sd, sd_mount（layout 按 feature 选尺寸）
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
 ├── pcf8563.rs ◄── i2c_bus（v2 编译进，v1 不使用）
 ├── aht20.rs   ◄── i2c_bus（v2 编译进）
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
  "-C", "link-arg=-Tlinkall.x",
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

feature 为**双维度**：`epd_*` 选屏幕驱动，`board_*` 选板硬件，一个设备 = epd + board 组合。

```toml
[features]
epd2in9 = []           # 屏：2.9 寸 (296×128) — 天气/日历页为横屏适配，排版未精细调整
epd2in7 = []           # 屏：2.7 寸 (264×176)
epd4in2 = []           # 屏：4.2 寸 (400×300)
board_v1 = []          # 板：v1 旧板（双电源轨，键1 直连 GPIO9 + 键2/3 走 GPIO2 ADC 梯，无 I2C）
board_v2 = []          # 板：v22 新板（单电源轨，三键 GPIO4 ADC 梯，I2C 挂 PCF8563 + AHT20，仅配 epd4in2）
enable_debug = []      # 启动时若 Flash 有错误日志则进入调试页
weather-openmeteo = [] # 天气数据源切换为 Open-Meteo（默认心知天气）
ttf_spike = []         # TTF 基准测试（dep:ab_glyph，不影响正常固件）
```

`src/board/mod.rs` 在编译期用 `compile_error!` 守卫非法组合：屏型三选一、板型二选一、`board_v2` 只能配 `epd4in2`。

```bash
cargo run --release --features epd4in2,board_v2                # v22 新板 4.2 寸版本
cargo run --release --features epd4in2,board_v1                # v1 旧板 4.2 寸版本
cargo run --release --features epd2in7,board_v1                # v1 旧板 2.7 寸版本
cargo run --release --features epd2in9,board_v1                # v1 旧板 2.9 寸版本
cargo run --release --features "epd4in2,board_v2,enable_debug"  # 带调试模式
```

> 固件体积已超过 debug 默认配置，必须用 `--release` 才能链接进 3MB factory 分区。`epd2in9` 的天气/日历页是按 296×128 横屏适配的，功能正常但排版未精细调整。

### 3.5 核心依赖说明

```toml
# ── ESP 生态 v1.x ──
esp-hal = { version = "1.1.1", features = ["esp32c3", "unstable", "log-04"] }
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
epd-waveshare = { git = "https://github.com/longxiangam/epd-waveshare-modify" }   # 自定义 fork
u8g2-fonts = { version = "0.7.1" }
ab_glyph_rasterizer = { version = "0.1.10", features = ["libm"] }  # TTF 覆盖率栅格化

# ── 存储（embedded-sdmmc 用 fork lfn-create 分支：支持中文文件名/目录创建）──
embedded-sdmmc = { git = "https://github.com/longxiangam/embedded-sdmmc-rs", branch = "lfn-create" }

# ── 网络 ──
reqwless = "0.14.0"
embedded-tls = "0.18"
no-std-net = "0.6"
sntpc = "0.3"
mini-json = { git = "https://github.com/longxiangam/mini-json" }   # 自定义 JSON 解析
```

**与旧版主要变化：**

| 旧版 (esp-hal 0.19) | 新版 (esp-hal 1.x) | 说明 |
|---------------------|---------------------|------|
| `esp-hal` 0.19 | `esp-hal` 1.1.1 | HAL 主版本升级 |
| `esp-wifi` 0.7 | `esp-radio` 0.18 | WiFi 驱动重命名重写 |
| `esp-hal-embassy` 0.2 | `esp-rtos` 0.3 | RTOS 集成层 |
| `embedded-alloc` | `esp-alloc` 0.10 | ESP 专用分配器 |
| *(无)* | `esp-bootloader-esp-idf` 0.5 | 新增引导加载器支持 |
| `embassy-executor` 0.6 | `embassy-executor` 0.10 | 任务调度器升级 |
| `embassy-net` 0.4 | `embassy-net` 0.9 | 网络栈重大升级 |
| `embassy-sync` 0.6 | `embassy-sync` 0.8 | 同步原语升级 |
| `embedded-hal-bus` 0.1 | `embedded-hal-bus` 0.3 | HAL 总线升级 |
| `reqwless` 0.11 | `reqwless` 0.14 | HTTP 客户端升级 |
| `embedded-sdmmc` 0.9 (crates.io) | fork `lfn-create` 分支 | 新增 LFN（中文长文件名）文件/目录创建 |
| *(无)* | `no-std-net` 0.6 | 新增网络地址类型 |
| *(无)* | `ab_glyph_rasterizer` 0.1 | 新增 TTF 矢量栅格化 |
| `[patch.crates-io]` 大量补丁 | 无补丁（fork 直接指向 git 仓库） | 直接使用上游 crate / 自维护 fork |
| edition = "2021" | edition = "2024" | Rust 版本升级 |

---

## 4. 模块结构

```
src/
├── main.rs              # 程序入口，硬件初始化，主循环（含阅读/正常重启分模式）
├── panic.rs             # 自定义 panic 处理器（崩溃写 Flash）
├── board/               # 板级抽象（epd_* 选屏 + board_* 选板，编译期固定 IO）
│   ├── mod.rs           #   屏型/板型 compile_error! 守卫、两板相同的 EPD/SPI/SD 引脚、诊断
│   ├── v1.rs            #   v1 旧板：双电源轨、GPIO9 直连键 + GPIO2 ADC 梯、无 I2C
│   └── v2.rs            #   v22 新板：单电源轨、GPIO4 三键梯、I2C（PCF8563 + AHT20）
├── display.rs           # 电子墨水屏渲染服务（独立 Embassy task）
├── event.rs             # 事件系统（按键检测 + 事件队列 + 发布-订阅派发）
├── pcf8563.rs           # PCF8563 硬件 RTC（走时/倒计时/绝对报警，v2）
├── aht20.rs             # AHT20 温湿度传感器驱动 + 周期采样任务（v2）
├── i2c_bus.rs           # I2C 总线容器（Mutex 全局 + write/write_read 助手，v2）
├── sleep.rs             # 深度睡眠与唤醒管理（PCF 分档布防 + reboot_sleep）
├── wifi.rs              # WiFi STA/AP 模式（基于 esp-radio）
├── worldtime.rs         # NTP 时间同步，Clock 全局时钟（对时结果写 PCF8563）
├── weather.rs           # 天气/节假日数据请求与缓存（双数据源 + 3 URL 回退）
├── location.rs          # 基于 IP 的自动定位（ip-api）
├── request.rs           # HTTP/HTTPS GET 客户端
├── battery.rs           # 电池电压 ADC 采样
├── storage.rs           # Flash 持久化存储（NVS 区域）
├── sd_mount.rs          # SD 卡挂载与文件操作封装（fork LFN：中文文件名创建）
├── ttf_sd.rs            # SD 卡 TTF 按需字形解析与栅格化（阅读模式专用）
├── epd2in9_txt.rs       # TXT 文本分页引擎（多屏尺寸）
├── flash_sleep.rs       # 待机壁纸的 Flash 存储与渲染
├── web_service.rs       # WiFi 配网的 Web 服务（书籍/图片/壁纸上传与管理 + JSON API）
├── random.rs            # 硬件 RNG → rand_core 封装
├── utils.rs             # make_static! 宏
│
├── model/               # 数据模型层
│   ├── mod.rs
│   ├── seniverse.rs     # 心知天气 API 数据结构
│   ├── open_meteo.rs    # Open-Meteo API 数据转换
│   ├── holiday.rs       # 节假日数据结构
│   ├── lunar.rs         # 农历/节气/星座算法
│   └── stock.rs         # 股票模型（KLine/ChartMode/RealtimeQuote + 解析 + 分时缓存）
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
    ├── mod.rs               # Page trait 定义，main_task / reading_task，PageEnum
    ├── main_page.rs         # 主菜单页面（页面管理器/默认主页/唤醒回上次页）
    ├── debug_page.rs        # 错误日志调试页
    ├── image_page.rs        # 图片浏览页面
    ├── setting_page.rs      # 设置/配网页面
    ├── read_menu_page.rs    # 占位（todo!()，未使用）
    ├── read/                # 电子书阅读（TTF 动态分页）
    │   ├── page.rs          #   ReadPage 状态机（书单/阅读/菜单/书签/排版/字体）
    │   ├── bitmap.rs        #   点阵字体位图绘制
    │   └── layout_*.rs      #   按 feature 选尺寸（400x300 / 176x264 / 128x296）
    ├── weather/             # 天气页面（layout_264x176 / layout_400x300 / layout_128x296 + render_data）
    ├── calendar/            # 农历日历页面（layout_* + render_data）
    └── stock/               # 股票页面（layout 单文件 + render_data）
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
DRAM2 堆区域注册（.dram2_uninit 段 ~64KB → esp_alloc::HEAP.add_region）
  │
hal_init(HalConfig::default().with_cpu_clock(CpuClock::max()))  ← HAL 初始化
  │
board::init_pad_hold()            ← v2 释放上次深睡 hold / v1 给 GPIO1 打 hold
  │
board::init_i2c()                 ← v2 建 I2C 总线（PCF8563+AHT20），v1 返回 None
  │
board::sd_miso_pullup()           ← GPIO10 内部上拉（SD 兼容，须在 SPI 占用前配置）
  │
board::init_power_rails()         ← 电源轨初始化（v2 单轨 GPIO1 / v1 双轨 GPIO21+GPIO1）
  │
TimerGroup + SoftwareInterruptControl     ← 配置 Embassy 定时器
  │
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0)  ← 启动 RTOS 调度
  │
Rtc::new(peripherals.LPWR)                ← 初始化 RTC
  │
storage::enter_process()                  ← 从 Flash 加载配置
  │
display::reload_full_refresh_times()      ← 加载全刷间隔缓存
  │
reset_reason / wakeup_cause 打印          ← 复位与唤醒原因
  │
pcf8563::consume_timer_wake()             ← 读 PCF TF/AF 判断定时唤醒并清标志
  │                                         （或 ESP RTC Timer 回退唤醒）
sleep::set_timed_wake(timed, 窗口)        ← 天气数据过期给 8s 刷新窗口，否则无窗口
  │
非定时唤醒 → display::mark_need_clear()   ← 首帧强制全刷清残影
  │
ADC 配置（按键梯 / 电池分压——引脚按板型 board::*） 
  │
按键数字输入 + 上拉（ADC 初始化之后再配一次）
  │
board::spawn_sensors()            ← v2 启动 AHT20 采样任务
  │
spawner.spawn(event::run)         ← 启动按键检测任务（v2 单梯 / v1 双路）
spawner.spawn(event::dispatch)    ← 启动事件派发任务
  │
GPIO2 深睡隔离（slp_sel=1 + IE/WPU/WPD=0，电池分压脚深睡高阻防漏电）
  │
SPI 初始化 (SpiConfig + Rate::from_mhz(32))  ← 创建 SPI 总线
  │
SPI 总线共享 (CsMutex + CriticalSectionDevice)  ← EPD/SD 共享
  │
spawner.spawn(display::render)            ← 启动显示渲染任务
  │
SdCard + VolumeManager                    ← 挂载 SD 卡
  │
add_rtcio(按键梯 GPIO, Low)               ← RTC IO 唤醒源（阅读模式睡眠也要靠它）
  │
阅读模式判定（rtc_fast READING_MODE 或 NVS 上次在阅读）
  │
  ├── 阅读模式 → worldtime::init_clock_pcf()（PCF 恢复时钟）
  │              + spawn(battery) + spawn(pages::reading_task)，main 空转
  └── 正常模式
       ├── 未配置 WiFi → AP 模式配网（SettingPage 循环）
       └── 已配置 → STA 模式连接
            ├── spawn(battery::test_bat_adc)
            ├── spawn(worldtime::ntp_worker)
            ├── spawn(pages::main_task)
            └── connect_wifi()
```

#### 5.1.3 内存分配器（esp-alloc）

堆缓冲放进独立的 **dram2 段**（bootloader 释放的 ~64KB DRAM，原本空闲）：

```rust
#[unsafe(link_section = ".dram2_uninit")]
static mut DRAM2_HEAP: core::mem::MaybeUninit<[u8; 65280]> = core::mem::MaybeUninit::uninit();

// main 里注册为堆区域
esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(heap_ptr, 65280, ...));
```

**设计动机：**
- 主 DRAM 全部留给栈（TTF 渲染异步调用链很深，64KB 堆放主 DRAM 时曾栈溢出）
- 堆又够用（WiFi ~60KB / TtfWs ~44KB），且栈溢出看门狗（`_stack_end` 处 watchpoint）不受影响
- 阅读与正常模式共用这一块堆，无需按模式区分

#### 5.1.4 HAL 初始化

```rust
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let peripherals = hal_init(config);
```

esp-hal v1.x 中 `hal_init(HalConfig)` 一行完成所有初始化并返回 `Peripherals`，不再需要手动管理系统时钟控制器。

#### 5.1.5 GPIO 新 API

```rust
// 新版：直接从 peripherals 获取 GPIO
let epd_busy = peripherals.GPIO6;
let epd_cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
```

esp-hal v1.x 中 GPIO 引脚直接从 `Peripherals` 获取，不再需要中间 `Io` 对象。`Output::new` 增加 `OutputConfig` 参数。板型相关的按键/电池/I2C 引脚一律经 `crate::board::*` 获取（详见 5.12）。

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

`SpiConfig` 结构体配置 + `Rate::from_mhz(32)`，两代板共用同一条 SPI 总线与引脚。

#### 5.1.7 SPI 总线共享

```rust
use critical_section::Mutex as CsMutex;

let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);

let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();
```

`critical_section::Mutex` + `CriticalSectionDevice::new_no_delay()`（embedded-hal-bus 0.3）。

#### 5.1.8 Embassy RTOS 启动

```rust
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

#### 5.1.9 深睡唤醒分类与定时唤醒

开机时区分本次唤醒来源，决定"刷完显示立刻回睡"策略：

```rust
// PCF TF=1 → PCF 定时唤醒（PCF INT 拉低按键梯 GPIO）；ESP RTC Timer → 布防失败的回退唤醒
let timed_wake = crate::pcf8563::consume_timer_wake().await
    || matches!(wake_reason, esp_hal::system::SleepSource::Timer);
// 天气数据过期才需要联网刷新（8 秒窗口等 WiFi+同步）；数据新鲜则无窗口
let stale = crate::weather::Weather::weather_stale().await;
crate::sleep::set_timed_wake(timed_wake, if stale { 8_000 } else { 0 });
// 非定时唤醒：首帧强制全刷，清上一会话残影
if !timed_wake {
    crate::display::mark_need_clear();
}
```

#### 5.1.10 重启分模式（阅读 / 正常）

阅读模式是**重启分模式**：启动时不初始化 WiFi，独占整块堆做 TTF（大字形缓存 + 深栈）。

- 模式标志 `READING_MODE` 存 rtc_fast（跨 reboot_sleep 深睡保留）
- v2 板 IO（RTCIO）唤醒会丢 rtc_fast → 从 NVS `SleepPageStorage`（`page == 100` 哨兵）恢复"上次在阅读"
- 阅读模式没有 ntp_worker，用 `worldtime::init_clock_pcf()` 从 PCF8563 恢复时钟
- 退出阅读 → `reading_task` 清标志 → `reboot_sleep()`（50ms 短深睡）回正常模式

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
#[cfg(feature = "epd2in7")]
pub type EpdDisplay = Display2in7;          // 264×176
#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;          // 400×300

pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd4in2<SPI, BUSY, DC, RST, DELAY>; // 同样按 feature

// SPI 设备类型
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

GPIO 引脚是泛型参数化的 `esp_hal::peripherals::GPIOx<'static>`。两代板的 EPD 控制脚相同（busy=6 / rst=7 / dc=20）。

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

#### 5.2.7 渲染与睡眠协同

- `await_render_idle()`：睡眠/断电前等待 `RENDER_BUSY=false` 且 `RENDER_CHANNEL` 排空——异步刷新进行到一半断电会产生发灰残影（需全刷才能清除）
- `set_sleep_renderer()`：页面注册自己的"睡眠画面"渲染函数（如股票页渲染当前行情作为待机画面），`show_sleep()` 时调用；页面 run 退出时置回 None

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

#### 5.3.3 事件队列与派发（生产者-消费者）

按键扫描与回调执行解耦：扫描/状态机只管把事件入队，由独立的 `dispatch` 任务 FIFO 串行派发——**长回调执行期间扫描不停，按键不丢**。

```rust
/// 按键事件队列：容量 8。生产者=toggle_event（扫描/状态机），消费者=dispatch 任务。
static EVENT_QUEUE: Mutex<CriticalSectionRawMutex, Vec<EventType, 8>> = Mutex::new(Vec::new());
static EVENT_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// 事件入队（生产端）。KeyLongIng 连发在队尾合并；队列满丢最旧保最新。
pub async fn toggle_event(event_type: EventType, _ms: u64) {
    let mut q = EVENT_QUEUE.lock().await;
    if let EventType::KeyLongIng(k) = event_type {
        if let Some(EventType::KeyLongIng(k2)) = q.last() {
            if *k2 == k { q.pop(); }   // 队尾同键连发 → 原位替换，不堆积
        }
    }
    if q.len() >= q.capacity() { q.remove(0); }  // 满：丢最旧保最新
    q.push(event_type).ok();
    drop(q);
    EVENT_READY.signal(());
}

/// 事件派发任务：FIFO 串行派发到监听回调。
#[embassy_executor::task]
pub async fn dispatch() {
    loop {
        EVENT_READY.wait().await;
        loop {
            // 逐条取出 → dispatch_event（持 LISTENER 锁、回调串行执行）
        }
    }
}
```

约束：派发期间持有 `LISTENER` 锁且回调串行执行——回调内**禁止**调用 `on/clear/on_fixed` 重绑定事件（会死锁）；重绑定走页面 run 循环的约定不变。

#### 5.3.4 按键检测（按板型两套入口）

```rust
// v2 新板：三键共用 GPIO4（数字输入 + ADC 分压 + 深睡唤醒共用一 pad）
#[embassy_executor::task]
#[cfg(feature = "board_v2")]
pub async fn run(key2: crate::board::KeyLadderGpio) {
    let mut key_all = Input::new(key2, InputConfig::default().with_pull(Pull::Up));
    loop {
        key_all.wait_for_falling_edge().await;   // 按下都拉到数字低电平
        let num = judge_adc_num().await;         // 边沿后立即采样区分键号（中位数抗毛刺）
        if num == 0 { Timer::after_millis(10).await; continue; }
        key_detection(&mut key_all, num).await;
        refresh_active_time().await;
        crate::sleep::mark_user_touch();
    }
}

// v1 旧板：数字键 GPIO9（键1）+ 三键梯 GPIO2（键2/3 走 ADC），两路下降沿 select
#[embassy_executor::task]
#[cfg(feature = "board_v1")]
pub async fn run(key1: esp_hal::peripherals::GPIO9<'static>, key2: crate::board::KeyLadderGpio) { /* ... */ }
```

键号判定阈值按板型区分（`judge_adc_num`）：
- **v2**（GPIO4 梯，键1/2/3 都在梯上，实测 ≈44/409/645）：<220→键1，220~520→键2，520~900→键3
- **v1**（GPIO2 梯，只有键2/3 在梯上）：<200→键2，200~1000→键3，>1000 抖动忽略

每次实体按键都会 `sleep::mark_user_touch()`（退出"定时唤醒刷完即睡"模式，见 5.6）。

#### 5.3.5 ADC 类型变更

```rust
// ADC 引脚类型随板型走（board::KeyLadderGpio：v2=GPIO4 / v1=GPIO2）
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<
    AdcPin<crate::board::KeyLadderGpio, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>
>> = Mutex::new(None);

pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<
    Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>
>> = Mutex::new(None);
```

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

#### 5.5.2 存储布局

```
NVS 分区 (0x9000 起始)
  ├── VersionStorage        偏移 +0x00     版本号 + 初始化标记 (0x1234_abce)
  ├── WifiStorage           偏移 +sizeof    WiFi SSID + 密码 + 配置完成标记
  ├── WeatherStorage        偏移 +sizeof    天气 token + 城市 + 同步时间 + 缓存数据
  ├── SleepStorage          偏移 +sizeof    阅读睡眠 / 天气(日历)睡眠时长
  ├── OtherStorage          偏移 +sizeof    复用：存默认主页(0=电子书/1=天气/2=日历/3=股票/9=主界面)
  ├── HolidayStorage        偏移 +sizeof    节假日 token + 同步时间 + 缓存数据
  ├── ErrorLogStorage       偏移 +sizeof    错误计数 + 最后错误信息
  ├── StockStorage          偏移 +sizeof    最多 6 对 (代码,名称) + count + selected
  ├── DisplayStorage        偏移 +sizeof    全刷间隔 (0=未配置，display 侧 clamp)
  ├── ReadingSettings       偏移 +sizeof    阅读排版：字号 + 行距(i8) + 旋转 + 字体文件名(32B)
  ├── SyncStampStorage      偏移 +sizeof    "SYND"：手动对时/节假日更新时间戳 + 时间显示开关
  ├── SleepPageStorage      偏移 +sizeof    "PAGE"：睡眠前最后所在页（100=阅读模式哨兵）
  ├── ReadingFlagsStorage   偏移 +sizeof    "PGSW"：阅读按键 1/2 方向互换
  └── StrokeConfigStorage   偏移 +sizeof    "STKE"：阅读笔画加粗系数 ×10 (10..=30)

Storage 分区 (0x310000 起始)
  └── 待机壁纸图片       8 字节头 + 像素数据
```

- `StockStorage` 起的分区都是**后加的，追加在末尾、不动既有偏移**，因此无需改 `INIT_TAG`——旧固件留下的原始字节由使用方钳制或用 magic 字段校验（`SYND`/`PAGE`/`PGSW`/`STKE`）
- `SyncStampStorage` 复用原 ADC 校准槽（该机制已随固定阈值按键方案移除）
- `SleepPageStorage`：页面变化时同步落 NVS——IO（RTCIO）唤醒时 rtc_fast 状态不可靠，靠它恢复"最后所在页"与"上次在阅读"

#### 5.5.3 序列化与宏

通过 `unsafe` 指针转换实现零拷贝序列化，`impl_storage!` 宏自动为结构体实现读写方法。`HolidayStorage` 使用分块写入（每块 128 字节）避免栈溢出。`write_sleep_page` 等高频写入点在值未变化时跳过写 Flash，避免磨损。

### 5.6 sleep.rs — 低功耗管理

**文件路径：** `src/sleep.rs`

#### 5.6.1 深度睡眠流程

```
to_sleep_tips(sleep_time, idle_time, show_sleep)
  │
  timed_hold_off()？── 是 → 直接返回（定时唤醒的数据刷新窗口内不放行睡眠）
  │
  immediate 判定：TIMED_WAKE && !USER_TOUCHED && sleep_time > 0
  │     （定时唤醒后用户没碰过键 + 页面刷完渲染 → 立刻入睡，不再等空闲时间）
  │
  非 immediate：空闲时间未到 → 返回
  │
  force_stop_wifi()
  │
  定时唤醒分档布防（sleep_time > 0，见 5.6.3）
  │
  show_sleep → show_sleep() 显示待机画面（页面注册的 sleep_renderer）
  │
  await_render_idle() 等渲染周期结束并排空（中途断电会产生发灰残影）
  │
  WHEN_SLEEP_RTC_MS = get_rtc_ms()   ← rtc_fast，供 get_sleep_ms 估算睡时长
  │
  电源轨断电（EINK_PWER_PIN / SD_PWER_PIN take 后置高；空槽自动跳过）
  │
  save_time_to_rtc() 保存时间戳
  │
  Rtc.sleep_deep() 进入深度睡眠
```

唤醒后的时间估算：`get_sleep_ms() = 当前 RTC ms - WHEN_SLEEP_RTC_MS`，配合 `WHEN_SLEEP_TIME_TIMESTAMP`（rtc_fast）推算当前时刻。

#### 5.6.2 定时唤醒状态与刷新窗口

```rust
static mut TIMED_WAKE: bool = false;      // 本次唤醒是否为定时唤醒
static mut TIMED_HOLD_OFF_UNTIL: u64 = 0; // 数据刷新窗口截止时刻（ms）
static mut USER_TOUCHED: bool = false;    // 定时唤醒后用户是否按过键
```

- `set_timed_wake(timed, hold_off_ms)`：main 启动时设置。天气数据过期给 8s 窗口（等 WiFi/天气时间同步/AHT20 首采），数据新鲜无窗口（读完温度立刻睡）；同时清 `USER_TOUCHED`
- `timed_hold_off()`：窗口期内 `to_sleep_tips` 不放行睡眠
- `idle_for(default)`：窗口期内返回 1 小时（永不满足空闲），否则用页面配置值
- `mark_user_touch()`：`event::run` 每次实体按键调用——置位后退出"刷完即睡"，回退普通空闲流程

#### 5.6.3 定时唤醒分档布防（PCF8563 优先，ESP RTC 兜底）

```
sleep_time = 0            → 不布防（按钮唤醒睡眠）
≤ 255 s                   → PCF 倒计时 1Hz 秒档（arm_timer_secs）
≤ 255 min                 → PCF 倒计时 1/60Hz 分档（arm_timer_minutes，向上取整，最多多睡 59s）
> 255 min（有 PCF）        → PCF 绝对时间报警（arm_alarm_at，按 PCF 自身走时设定 UTC 时刻）
                            （纽扣电池 32k 晶振走时精确，主控深睡慢时钟漂移大）
PCF 布防失败 / 无 PCF(v1)  → TimerWakeupSource（ESP RTC 定时）兜底
```

唤醒源：按键梯 GPIO（RtcioWakeupSource，Low 电平）+ 定时源。PCF INT 接按键梯同一引脚（v2 = GPIO4），到点拉低即唤醒 CPU。

#### 5.6.4 reboot_sleep — 模式切换

```rust
pub async fn reboot_sleep() { /* 50ms 定时器唤醒的短深睡 */ }
```

"重启"切换模式：深睡唤醒会重新跑 main（重新决定起不起 WiFi、重新分配堆），且**保留 rtc_fast**（模式标志/书索引/续读偏移），所以模式切换不需要 software_reset（那会清 rtc_fast）、也不需要 flash。EINK/SD 电源按正常深睡一样关掉，唤醒后 main 重新上电。

### 5.7 worldtime.rs — NTP 时间同步

**文件路径：** `src/worldtime.rs`

#### 5.7.1 网络地址类型

```rust
use no_std_net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
```

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

#### 5.7.3 时间戳安全性改进

```rust
// 增加有效性检查
if ts > 1577836800 && ts < 4102444800 {  // 2020-2100 范围
    if let Ok(now) = OffsetDateTime::from_unix_timestamp(current_second as i64) {
        clock.set_time(now).await;
    }
}
```

#### 5.7.4 PCF8563 时钟源（恢复与写入）

- **恢复**：`ntp_worker` 启动时先 `pcf_restore()`——从 PCF8563 读 UTC 时间恢复 Clock（备电池维持，权威）；失败再退回 rtc_fast 估算（`WHEN_SLEEP_TIME_TIMESTAMP + get_sleep_ms()`，带 sanity 校验）
- **写入**：NTP 对时成功后 `pcf8563::write_time(ts)` 把 UTC 时间写进 PCF8563（STOP=1 暂停走时防回滚撕裂，写完清 STOP），并记录 `SyncStampStorage` 手动对时时间戳
- **阅读模式**：无 ntp_worker，main 直接调 `init_clock_pcf()` 从 PCF 恢复时钟——时间显示/跨天判断不再因无 WiFi 而滞后（股票跨天判断曾因此出错）

其它要点：
- NTP 服务器池 `["ntp.aliyun.com", "ntp.tuna.tsinghua.edu.cn", "ntp1.aliyun.com"]` 循环遍历，**DNS 失败即切下一台**，越界才返回错误（不再单点卡死）。
- `TimestampGen::timestamp_sec()` 必须返回 UNIX 秒（sntpc 内部再加 NTP 纪元差），误用微秒会让 roundtrip/offset 失真。
- `ntp_worker` 默认每小时（3600s）对时一次，失败退避（1s/10s，连续失败 >5 次拉长间隔），且对时成功后联动同步天气与节假日。
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

- `Stack<WifiDevice<'static, WifiStaDevice>>` 简化为 `Stack<'static>`
- TLS 使用 `UnsecureProvider::new::<Aes128GcmSha256>(&mut self.rng)`
- `reqwless` v0.14 的 Response API 有所变化

### 5.9 battery.rs — 电池管理

**文件路径：** `src/battery.rs`

```rust
#[ram(unstable(rtc_fast))]
static mut LAST_BATTERY_PERCENT: u32 = 0;
```

每 60 秒采样一次（`voltage = adc × 2`，分压系数；电量用平方曲线映射）。ESP32-C3 的 `read_oneshot` 在冷启动 / 通道切换时经常返回 `Err`，因此采用**出错重试、超限放弃**的策略：最多重试 50 次，期间保留上一次电量；超限才跳过本轮，避免死循环 + 持锁。`LAST_BATTERY_PERCENT` 镜像到 rtc_fast，`last_percent()` 供 `sleep_renderer` 在渲染路径里同步读取（不必锁 Mutex）。电池分压脚按板型走 `board::BatAdcGpio`（v2=GPIO2 / v1=GPIO4）。

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

### 5.11 其余模块

| 模块 | 说明 |
|------|------|
| `location.rs` | 基于 IP 的自动定位：请求 `ip-api.com`（明文 HTTP）反查城市与经纬度，产出 `"lat:lon"` 供心知天气查询。走标准 `use_wifi` 三段式 |
| `weather.rs` | 双数据源（`weather-openmeteo` feature 切换）；`request()` 统一入口；节假日源 3 URL 回退（`cdn.jsdelivr.net` → `fastly.jsdelivr.net` → `raw.githubusercontent.com`，境内 SNI 阻断 raw 源，境外仍可用），最多重试 3 次、失败冷却 60s；rtc_fast 跨深睡标志；`weather_stale()` / `stale_in_secs()`（5h 同步间隔）供睡眠调度 |
| `epd2in9_txt.rs` | TXT 分页引擎，按 feature 给出 2.9/2.7/4.2 三套 `LINES_NUM/WIDTH/HEIGHT`；`.log` 文件语义：`[0]` = 上次阅读字节偏移（bit31=已到末页），`[1..]` = 书签字节偏移 |
| `sd_mount.rs` | embedded-sdmmc 封装；LFN（中文长文件名）由 **fork `lfn-create` 分支**提供（旧版手写 LFN 目录项代码已删除）；书名最长 150 字节（~40 汉字） |
| `ttf_sd.rs` | SD 卡 TTF 按需随机读取字形（不整文件载入）：自实现 TTF 解析（表目录→cmap→loca→hmtx→glyf）逐字形 seek+read，二次曲线经 ab_glyph_rasterizer 覆盖率光栅化；cmap 全子表缓存（28KB）+ loca/glyf SD 回退；工作区 `TtfWs` 堆分配（进阅读分配、退出释放，与 WiFi 堆内存运行期互斥）；支持笔画加粗系数（修细笔画断墨） |
| `flash_sleep.rs` | 待机壁纸 Flash 存储（魔数 `"SLEP"` + 崩溃安全写入）；BMP→1bit 转换（ITU-R BT.601 灰度）；300×400 分辨率 |
| `web_service.rs` | 配网 Web 服务（见 5.4.5）；路由覆盖书籍/图片/壁纸的列表、上传、删除，WiFi/天气/睡眠/显示/股票/阅读配置表单，`/config` JSON API；按 Content-Length 跨分段读 body（大文件上传不丢字段） |
| `random.rs` | `esp_hal::rng::Rng` → `rand_core::RngCore + CryptoRng` 封装，供 TLS/二维码使用 |

### 5.12 board/ — 板级抽象

**文件路径：** `src/board/{mod.rs, v1.rs, v2.rs}`

feature 双维度在编译期固定整台设备的 IO 分配：`epd_*` 选屏幕驱动，`board_*` 选板硬件。`mod.rs` 用 `compile_error!` 守卫非法组合（屏型三选一、板型二选一、v2 只配 epd4in2），并通过 `pub use v1::*` / `pub use v2::*` 统一再导出——调用方一律 `board::xxx`，不感知具体板文件。

**编译矩阵：**

| 组合 | 命令 |
|------|------|
| v2 新板 4.2 | `--features epd4in2,board_v2` |
| v1 旧板 4.2 | `--features epd4in2,board_v1` |
| v1 旧板 2.9 | `--features epd2in9,board_v1` |
| v1 旧板 2.7 | `--features epd2in7,board_v1` |

**两板差异：**

| 项目 | board_v1（旧板） | board_v2（v22 新板） |
|------|-----------------|---------------------|
| 按键 | 键1 = GPIO9 数字直连；键2/3 = GPIO2 ADC 梯（兼深睡唤醒） | 键1/2/3 全在 GPIO4 ADC 梯（数字输入 + ADC + 深睡唤醒 + PCF INT 共用一 pad） |
| 电池 ADC | GPIO4 | GPIO2 |
| 电源轨 | 双轨：EINK=GPIO21 / SD=GPIO1，**高电平断电**（初始 High，上电拉低） | 单轨：GPIO1，**低电平开启**（初始 Low；EINK/SD 两槽填同一轨，睡眠统一置高断轨） |
| I2C | 无（`init_i2c` 返回 None） | GPIO9=SDA / GPIO21=SCL，挂 PCF8563(0x51) + AHT20(0x38) |
| PCF8563 | `HAS_PCF=false`：定时唤醒走 ESP RTC，时钟回落 rtc_fast 估算 | `HAS_PCF=true`：定时唤醒/时钟源优先 PCF |
| AHT20 | `HAS_AHT20=false` | `HAS_AHT20=true`（spawn_sensors 启动采样任务） |
| pad hold | 深睡前给 GPIO1(SD 轨) 打 `rtcio_pad_hold`（深睡中保持轨状态） | 开机清 `PAD_HOLD` 寄存器（释放上次深睡布下的 hold——不释放则 GPIO1 驱低写不进去 → 轨断电 → 显示/SD 全灭） |

**两板相同（mod.rs 统一定义）：** EPD/SPI/SD 引脚（busy=6 / rst=7 / dc=20 / cs=3 / sck=8 / mosi=0 / miso=10 / sd_cs=5）、`sd_miso_pullup()`（MISO 内部上拉，SD 未初始化前 DO 高阻，须在 SPI 占用 GPIO10 前配置）、`RTC_FAST_BOOT_SEQ` 诊断计数（rtc_fast 自增，跨深睡递增 = 保留正常）。

### 5.13 pcf8563.rs — 硬件 RTC（v2）

**文件路径：** `src/pcf8563.rs`

PCF8563（I2C 0x51，CR2032 备电池维持）承担两个角色：**系统时钟源**与**深睡定时唤醒源**。时间以 UTC 存取（`Clock::local` 负责 +8h 时区）。

**走时：** 秒(0x02, bit7=VL 无效标志) 分 时 日 星期 月(bit7=世纪) 年，全部 BCD。
- `read_time()`：读 7 字节转 Unix 秒；VL=1（备用电池掉过电/首次上电）或字段越界返回 None
- `write_time(ts)`：写入前置 STOP=1 暂停走时防回滚撕裂，写完清 STOP
- `ts_valid()`：2020-2100 区间校验，与世界时间模块一致

**倒计时定时器**（寄存器 0x0E Timer_control / 0x0F Timer_A，到 0 时 TF 置位、INT 拉低）：
- `arm_timer_secs(n)`：1Hz 档，1..=255 秒后 INT 拉低
- `arm_timer_minutes(n)`：1/60Hz 档，最大 255 分钟（≈4.25 小时），适合天气刷新等长周期
- `disarm_timer()`：唤醒后 TE=0，防止运行期 INT 拉低按键网络造成假按键
- `consume_timer_wake()`：读 TF/AF 判断是否定时唤醒，并清标志 + 关定时器/中断使能

**绝对时间报警**（寄存器 0x09 起，分/时/日 BCD 比较 + 星期写 0x80 禁用）：`arm_alarm_at(ts)` 按 PCF 自身走时设定 UTC 时刻，用于超过倒计时上限（255 分钟）的长间隔。报警无月份寄存器：间隔 >28 天时会在次月同日提前唤醒一次（醒来重新布防即可，无害）。

### 5.14 i2c_bus.rs 与 aht20.rs（v2）

**`i2c_bus.rs`**：I2C 总线容器。`I2C: Mutex<Option<I2cDev>>` 全局持有总线，`write()` / `write_read()` 助手函数供 PCF8563 与 AHT20 驱动共享总线（互斥加锁）。PCF8563 安全上限 100kHz。

**`aht20.rs`**：温湿度传感器（0x38）驱动 + 周期采样任务。
- 协议：上电等 40ms → 状态字 bit3=已校准（否则发 0xBE 初始化）→ 测量 `[0xAC,0x33,0x00]` → 等 80ms → 读 7 字节（状态+湿度20bit+温度20bit+CRC8，poly 0x31）
- 全整数运算无浮点；数据存原子量（`AtomicI32/AtomicU32`），渲染侧 `indoor_snapshot()` 同步读取快照（无锁）
- `sample_task`：唤醒期间每 60s 采样；唤醒后首测常因传感器忙失败 → 快速重试 3 次（间隔 200ms）；连续失败清空显示避免陈旧值

### 5.15 ttf_sd.rs — TTF 矢量字体渲染

**文件路径：** `src/ttf_sd.rs`

SD 卡 TTF **按需随机读取字形**（不整文件载入），供阅读模式渲染任意中文字体：

- 自实现 TTF 解析链：表目录 → cmap → loca → hmtx → glyf，逐字形从 SD seek+read
- 二次曲线经 `ab_glyph_rasterizer` 覆盖率光栅化（自适应展平 + 像素覆盖率阈值），no_std + libm 软浮点（riscv32imc 无 FPU）
- 缓存策略：`FontInfo`（表偏移/度量）首次解析后常驻 TtfWs，避免每页翻页重复读表；cmap 全子表缓存（28KB，cmap 是每字查询的瓶颈）；loca/hmtx 小缓存 + glyf 走 SD 回退；字形位图缓存为扁平缓冲（12KB 预算，按字号运行时切分槽位——大字号槽大槽少、小字号槽小槽多）
- 工作区 `TtfWs` **堆分配**：进阅读分配、退出释放，与 WiFi 的堆内存运行期互斥（阅读模式不连网），不再常驻 .bss
- 笔画加粗系数（×1.0~×3.0，像素覆盖率 × 系数判定）：修细笔画断墨，可在阅读排版菜单调节并持久化

---

## 6. 数据模型层

**目录路径：** `src/model/`

| 文件 | 功能 |
|------|------|
| `seniverse.rs` | 心知天气 API 数据结构，风向转换，蒲福风级 |
| `open_meteo.rs` | Open-Meteo API 数据转换（WMO → 心知天气格式） |
| `holiday.rs` | 节假日数据结构（含日期、名称、是否休息日） |
| `lunar.rs` | 农历/节气/星座算法（1900-2100 预计算数据，支持闰月） |
| `stock.rs` | 股票模型：`KLine`/`ChartMode`(6 模式环形)/`RealtimeQuote`/`StockMinuteCache`(分时缓存)，新浪 K 线与腾讯盘口解析、URL 构造、价格格式化 |

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

`MainPage` 维护菜单列表和当前选中页面，通过 `PageEnum` 路由。正常模式由 `main_task` 驱动；阅读模式是独立入口 `reading_task`（main 直接 spawn，见 5.1.10）：

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

reading_task（阅读重启分模式）
  → 等显示任务就绪 → ReadPage::new().bind_event().run()
  → 退出：清 READING_MODE + 主网格页设 -1 → reboot_sleep() 回正常模式
```

### 8.3 页面功能

| 页面 | 按键映射 | 主要功能 |
|------|---------|---------|
| **MainPage** | Key1/2 上下，Key3 确认 | 图标网格菜单导航；冷启动进默认主页、唤醒回上次页（rtc_fast + NVS 兜底） |
| **ReadPage** | Key1 下翻/向下，Key2 上翻/向上（"方向键互换"可对调），Key3 短按菜单/确认，长按3 退出，长按1/2 连发 | TTF 矢量字体阅读（SD 字体可选）+ 内置点阵；TTF 动态分页（前进 O(1)、后退走历史栈）；书单二级目录；菜单：返回书单/书签/跳转进度/排版/字体/旋转屏幕/方向键互换/睡眠；书签列表 + 打开/删除弹层；排版（字号 12~40 / 行距 ±10 / 笔画加粗 / 重置）；进度三位小数；阅读位置 `.log[0]` 持久化 |
| **WeatherPage** | Key1 刷天气，Key2 刷节假日，长按3 退出 | 天气概览、5 日预报、温度曲线、室内温湿度（v2）；睡眠时长：时间显示开 → 对齐下一分钟边界（唤醒即分钟跳变），关 → 睡到天气数据过期那一刻（5h 同步间隔对齐） |
| **CalendarPage** | Key1/2 短按切月，Key3 短按回今天，长按1 刷天气，长按2 刷节假日，长按3 退出 | 日历、农历、节气、节假日（休/班） |
| **StockPage** | 短按1/2：总览移格 / 明细切图模式；长按1：总览强制刷新 / 明细上一支；长按2：总览下一格 / 明细下一支；短按3：进明细 / 重新请求；长按3：逐级返回 | 总览（6 支分时缓存网格）/ 明细：分时/日K/周K/月K/折线/盘口；实时模式开盘时段 120s 唤醒刷新，非开盘（午休/盘前盘后/周末/节假日）睡到下一开盘时点；其它模式 12h |
| **ImagePage** | Key1/2 选择（长按连发），Key3 查看/菜单，长按3 退出 | BMP 图片浏览和壁纸设置 |
| **SettingPage** | Key1/2 调整（长按连发），Key3 确认，长按3 退出 | 阅读睡眠时长、天气睡眠时长、自动定位、web 配置、股票选择、默认主页、全刷次数、手动对时、节假日同步、时间显示 |
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
  → EVENT_QUEUE（FIFO，容量 8，KeyLongIng 队尾合并，满丢最旧）+ EVENT_READY
  → event::dispatch() 任务逐条取出 → Listener[] 回调串行执行
  → 修改页面状态 → render() → RENDER_CHANNEL → display::render()
```

生产者（扫描）与消费者（回调执行）解耦：长回调执行期间扫描不停，按键不丢。

### 9.3 全局静态状态

```rust
// 模式 1: Embassy Mutex + Option
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);

// 模式 2: core::ptr 安全访问 static mut
unsafe { core::ptr::addr_of_mut!(DISPLAY).write(Some(display)); }
unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut() }

// 模式 3: StaticCell + make_static!
let shared_spi_static = static_cell::make_static!(shared_spi);

// 模式 4: RTC 快速内存（跨深睡保留）
#[ram(unstable(rtc_fast))]
static mut PAGE_INDEX: i32 = 1;

// 模式 5: 原子量快照（渲染路径无锁读取，AHT20 室内温湿度）
static INDOOR_T: AtomicI32 = AtomicI32::new(i32::MIN);
```

### 9.4 条件编译

```rust
// 屏幕维度：布局/分页参数按 feature 选
#[cfg(feature = "epd2in9")]
const LINES_NUM: u32 = 7;

#[cfg(feature = "epd4in2")]
const LINES_NUM: u32 = 22;

// 板型维度：按键任务入口 / ADC 类型 / 传感器 / PCF
#[cfg(feature = "board_v2")]
pub async fn run(key2: crate::board::KeyLadderGpio) { /* 单梯 */ }

#[cfg(feature = "board_v1")]
pub async fn run(key1: GPIO9<'static>, key2: crate::board::KeyLadderGpio) { /* 双路 */ }

// 数据源维度
#[cfg(not(feature = "weather-openmeteo"))]
{ Self::request_seniverse(&weather_storage).await }

#[cfg(feature = "weather-openmeteo")]
{ Self::request_open_meteo(&weather_storage).await }
```

### 9.5 Channel / 队列解耦

| 通道 | 生产者 | 消费者 | 数据 |
|------|--------|--------|------|
| `EVENT_QUEUE` + `EVENT_READY` | event::run（按键扫描） | event::dispatch | `EventType` FIFO |
| `RENDER_CHANNEL` | 各页面 | `display::render` | `RenderInfo` |
| `QUICKLY_LUT_CHANNEL` | 页面 | `display::render` | `bool` |
| `STOP_WIFI_SIGNAL` | sleep/do_stop | connection_wifi | `()` |
| `RECONNECT_WIFI_SIGNAL` | use_wifi | connection_wifi | `()` |
| `REINIT_WIFI_SIGNAL` | use_wifi | connect_wifi | `()` |

---

## 10. 数据流

### 10.1 按键事件流

```
物理按键 → GPIO 下降沿 → event::run() → (ADC 区分按键号) → key_detection()
  → toggle_event() → EVENT_QUEUE（FIFO）
  → event::dispatch() 逐条派发 → Listener 回调（串行执行）
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

### 10.4 定时唤醒数据流

```
入睡（页面 sleep_time>0）→ sleep::to_sleep_tips 分档布防（PCF 倒计时/报警，失败回退 ESP RTC）
  → 深睡 → PCF INT 拉低按键梯 GPIO（或 ESP RTC 计时到）→ 唤醒重启
  → main: consume_timer_wake() 分类 → set_timed_wake(数据过期 ? 8s 窗口 : 0)
  → 页面 run：timed_hold_off 窗口内等 WiFi/同步/AHT20 → 刷新 + 渲染完成
  → 用户没碰过键 → to_sleep_tips 立即入睡（"刷完即睡"）
```

---

## 11. 内存管理

### 11.1 内存分区

```
ESP32-C3 内存 (~400KB SRAM)
  │
  ├── 全局堆 (~64KB)         dram2 独立段（.dram2_uninit，bootloader 释放的 DRAM）
  │     esp_alloc::HEAP.add_region(...) 注册
  │     用于 Vec, String, Box 等动态分配（WiFi ~60KB / TtfWs ~44KB 复用同一块）
  │
  ├── 主 DRAM                全部留给任务栈（TTF 渲染异步调用链深，曾栈溢出）
  │
  ├── Embassy 任务栈          由 executor 自动分配
  │
  ├── 静态变量 (.bss + .data)  全局 Mutex, Channel, Signal 等
  │
  └── RTC 快速内存             #[ram(unstable(rtc_fast))]
        睡眠期间保持供电的变量（页索引/续读偏移/模式标志/渲染计数/分时缓存等）
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
| `event::run` | main.rs | 按键检测与事件入队（按板型两个入口） |
| `event::dispatch` | main.rs | 事件队列 FIFO 串行派发到监听回调 |
| `battery::test_bat_adc` | main.rs | 电池 ADC 采样 (60s 周期) |
| `worldtime::ntp_worker` | main.rs | NTP 同步（写 PCF8563）+ 天气/节假日同步 |
| `aht20::sample_task` | board::spawn_sensors (仅 v2) | 室内温湿度采样 (60s 周期) |
| `pages::main_task` | main.rs | 主界面事件循环（正常模式） |
| `pages::reading_task` | main.rs (仅阅读模式) | 阅读模式入口（独占堆做 TTF） |
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
    │ (按键扫描)  │  │render      │  │            │
    └─────┬──────┘  └──────┬─────┘  └─────┬──────┘
          │ toggle_event   │               │ use_wifi()
          ▼                │               ▼
    ┌────────────┐         │        ┌────────────┐
    │ event::    │         │        │connection_ │
    │ dispatch   │         │        │wifi        │
    └─────┬──────┘         │        └──────┬─────┘
          │ 回调            │               │
          ▼                ▼               ▼
    ┌────────────┐  ┌────────────┐  ┌────────────┐
    │pages::     │  │ EPD 硬件    │  │ net_task   │
    │main_task / │  │            │  │ (Runner)   │
    │reading_task│  └────────────┘  └────────────┘
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

### 13.1 两代板相同的引脚（EPD / SPI / SD）

| GPIO | 方向 | 功能 | 备注 |
|------|------|------|------|
| GPIO0 | 输出 | EPD MOSI (SPI) | SPI 数据输出 |
| GPIO3 | 输出 | EPD CS (SPI 片选) | 高电平不选中 |
| GPIO5 | 输出 | SD 卡 CS (SPI 片选) | 高电平不选中 |
| GPIO6 | 输入 | EPD BUSY | 屏幕忙状态 |
| GPIO7 | 输出 | EPD RST | 屏幕复位 |
| GPIO8 | 输出 | EPD SCLK (SPI 时钟) | SPI 时钟线 |
| GPIO10 | 输入 | EPD/SD MISO (SPI) | 内部上拉（SD 未初始化前 DO 高阻） |
| GPIO20 | 输出 | EPD DC (数据/命令) | SPI DC 信号 |

### 13.2 board_v1（旧板）

| GPIO | 方向 | 功能 | 备注 |
|------|------|------|------|
| GPIO1 | 输出 | SD 卡电源控制 | 高电平断电（深睡前打 pad hold 保持状态） |
| GPIO2 | 输入 | 按键 2/3（ADC 梯） | 多按键 ADC 检测，兼深睡唤醒（Low） |
| GPIO4 | 输入 | 电池 ADC | 电池电压分压检测 |
| GPIO9 | 输入 | 按键 1 | 数字直连，上拉输入 |
| GPIO21 | 输出 | 墨水屏电源控制 | 高电平断电 |

### 13.3 board_v2（v22 新板）

| GPIO | 方向 | 功能 | 备注 |
|------|------|------|------|
| GPIO1 | 输出 | 单电源轨（EINK + SD） | 低电平开启，睡眠置高断轨；开机先释放 pad hold |
| GPIO2 | 输入 | 电池 ADC | 分压检测；深睡隔离（slp_sel=1 + IE/WPU/WPD=0）防漏电 |
| GPIO4 | 输入 | 按键 1/2/3（ADC 梯） | 数字输入 + ADC + 深睡唤醒 + PCF INT 共用一 pad |
| GPIO9 | I/O | I2C SDA | 挂 PCF8563 (0x51) + AHT20 (0x38) |
| GPIO21 | I/O | I2C SCL | 100kHz（PCF8563 安全上限） |

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
| 堆分配 | 手动 `embedded_alloc::Heap` + `init()` | dram2 段 + `esp_alloc::HEAP.add_region(...)` |
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

# 构建并烧录 v22 新板 4.2 寸版本（.cargo/config.toml 已配置 espflash 为 runner）
cargo run --release --features epd4in2,board_v2

# 构建 v1 旧板 4.2 / 2.7 / 2.9 寸版本
cargo run --release --features epd4in2,board_v1
cargo run --release --features epd2in7,board_v1
cargo run --release --features epd2in9,board_v1

# 带调试模式（启动时若 Flash 有错误日志则进入调试页）
cargo run --release --features "epd4in2,board_v2,enable_debug"
```

> 必须用 `--release`（`[profile.release] opt-level = "s"`），否则 `.text` 会溢出 3MB factory 分区。
