# Part 3: E-Paper Display Driver and Rendering Architecture

> Building an ESP32-C3 E-Paper Reader in Rust · Article Series
>
> 中文版: [../03-电子墨水屏驱动与渲染架构.md](../03-电子墨水屏驱动与渲染架构.md)

## Introduction

E-paper (E-Paper Display, EPD) is a very special display technology. Unlike LCD or OLED, it does not emit light; instead, it presents images by using electric fields to move charged pigment particles inside microcapsules. Its core advantage: **the image survives power loss** — which is exactly why e-book readers choose it.

But this property also brings unique engineering challenges: slow refresh (hundreds of milliseconds to seconds), "ghosting", and refresh strategies that must be carefully designed. This article walks through the full stack, from the driver layer to the rendering architecture, and explains how this project tames e-paper.

## 1. How E-Paper Works

### 1.1 Basic Principle

Each pixel of an e-paper panel contains a transparent microcapsule holding positively charged white particles and negatively charged black particles. Applying a positive voltage moves the white particles to the top and the pixel shows white; a negative voltage does the opposite.

This means:
- **Holding an image costs no power** — the particles stay where they are
- **Changing an image costs power** — voltage is needed to move particles
- **Refresh is slow** — physically moving particles takes time

### 1.2 Full Refresh and Partial Refresh

The Waveshare panels used in this project support two refresh modes:

| Mode | Duration | Characteristics |
|------|----------|-----------------|
| **Full** | ~2-3 s | Black/white flashing, completely clears ghosting |
| **Quick (partial)** | ~0.3 s | No flashing, but accumulates ghosting |

Ghosting is unique to e-paper: the previously displayed content faintly remains visible on the new frame. Partial refresh is fast, but every few refreshes a full refresh is needed to clear the ghosts.

## 2. The Display Driver Layer

### 2.1 Selecting the Panel via Conditional Compilation

The project supports multiple panel sizes through feature flags, unified by type aliases in `display.rs`:

```rust
// src/display.rs:33-46
#[cfg(feature = "epd2in9")]
pub type EpdDisplay = Display2in9;          // 296×128 pixels
#[cfg(feature = "epd2in9")]
pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd2in9<SPI,  BUSY, DC, RST, DELAY>;

#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;          // 400×300 pixels (driver from the epd4in2_v3 module)
#[cfg(feature = "epd4in2")]
pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd4in2<SPI, BUSY, DC, RST, DELAY>;

#[cfg(feature = "epd2in7")]
pub type EpdDisplay = Display2in7;          // 264×176 pixels
```

Panel and board features are guarded in `src/board/mod.rs`: one-of-three panel features (epd2in9 / epd2in7 / epd4in2) plus one-of-two board features (board_v1 / board_v2), fixing the entire IO assignment at compile time. Upper layers uniformly use the `EpdDisplay` and `EpdControl` types and never care about the specific panel model. Switching panels only requires changing build flags.

### 2.2 Display Initialization

The panel driver comes from the `epd-waveshare` crate (custom fork: `longxiangam/epd-waveshare-modify`). The EPD controller is initialized inside the render task:

```rust
// src/display.rs:114-131
#[embassy_executor::task]
pub async fn render(
    mut spi_device: &'static mut ActualSpi<'static>,
    busy: Input<'static>,
    rst: Output<'static>,
    dc: Output<'static>,
)
{
    // The caller (main) builds the GPIO drivers per feature; this task is not
    // tied to specific pin numbers
    let mut epd = EpdControl::new(&mut spi_device, busy, dc, rst, &mut embassy_time::Delay).unwrap();
    let mut display: EpdDisplay = EpdDisplay::default();
    display.clear_buffer(Color::White);

    let receiver = RENDER_CHANNEL.receiver();
    let quickly_lut = QUICKLY_LUT_CHANNEL.receiver();
    unsafe {
        core::ptr::addr_of_mut!(DISPLAY).write(Some(display));
    }
    // ...
}
```

The GPIO drivers are constructed in `main` and handed to the render task:

```rust
// src/main.rs:274-280
let epd_busy_in = Input::new(
    epd_busy,
    esp_hal::gpio::InputConfig::default().with_pull(Pull::Down),
);
let epd_rst_out = Output::new(epd_rst, Level::High, OutputConfig::default());
let epd_dc_out = Output::new(epd_dc, Level::High, OutputConfig::default());
spawner.spawn(display::render(spi_bus_epd, epd_busy_in, epd_rst_out, epd_dc_out).unwrap());
```

The EPD controller needs 4 GPIOs (identical on both board generations, defined in `src/board/mod.rs`):
- **BUSY** (GPIO6, pull-down input): busy signal of the panel (driven high while refreshing, low when done)
- **RST** (GPIO7): reset
- **DC** (GPIO20): data/command select
- **CS** (GPIO3): chip select; data transfer goes over the shared SPI bus described earlier

Once initialization completes, the framebuffer is stored in the global `static mut DISPLAY` for page code to draw into.

### 2.3 The SPI Device Type

The SPI device type used by the EPD is defined like this under esp-hal v1.x:

```rust
// src/display.rs:83
type ActualSpi<'a> = CriticalSectionDevice<'a, Spi<'a, esp_hal::Blocking>, Output<'a>, embedded_hal_bus::spi::NoDelay>;
```

The type looks complicated but decomposes clearly:
- `CriticalSectionDevice`: SPI device with critical-section mutual exclusion (covered in the previous article)
- `Spi<'a, esp_hal::Blocking>`: blocking-mode SPI bus
- `Output<'a>`: CS chip-select pin
- `NoDelay`: no delay between CS assertion and data

## 3. Producer-Consumer Rendering Architecture

### 3.1 Architecture

One of the biggest architectural highlights of this project is the **decoupled design** of the display system. Page code never touches the panel hardware directly; it sends render requests through a Channel:

```
┌──────────────┐    RENDER_CHANNEL    ┌──────────────┐    SPI    ┌─────────┐
│  Pages       │ ──── RenderInfo ──► │ render task  │ ────────► │ EPD     │
│  (producer)  │                      │ (consumer)   │           │ panel   │
└──────────────┘                       └──────────────┘           └─────────┘
```

The Channel definitions:

```rust
// src/display.rs:74-75
pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex, RenderInfo, 64> = Channel::new();
pub static QUICKLY_LUT_CHANNEL: Channel<CriticalSectionRawMutex, bool, 64> = Channel::new();
```

What a render request carries:

```rust
// src/display.rs:28-31
pub struct RenderInfo{
    pub time:i32,
    pub need_sleep:bool,
}
```

### 3.2 Why This Design?

**Decouples pages from hardware**. Page code only needs to: draw into the framebuffer → send a render request. It does not need to know the SPI bus exists, whether the panel is busy, or wait for the refresh to finish.

**Avoids blocking**. An e-paper refresh takes hundreds of milliseconds to seconds. If page code waited directly for the refresh, all UI logic would be blocked. Through the Channel, page code returns immediately after sending the request and can handle the next event.

**Serializes access**. The Channel guarantees only one render request is processed at a time, avoiding conflicts when multiple pages would otherwise touch the panel concurrently.

### 3.3 How Do Pages Draw?

The rendering flow of page code (using the reading page's `render()` in `src/pages/read/page.rs` as an example):

```rust
// Typical page rendering flow (simplified)
async fn render(&mut self) {
    if self.need_render {
        self.need_render = false;
        if let Some(display) = display_mut() {       // get the framebuffer
            display.clear_buffer(Color::White);      // clear

            // Draw with embedded-graphics (text / lists / progress bar …)
            list_widget.draw(display);

            // Vector-font text rendering (see Part 10)
        }

        // Send the render request (the reading page sleeps right after
        // rendering → need_sleep: true)
        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
    }
}
```

`display_mut()` returns a mutable reference to the framebuffer that pages can draw into freely. Once drawing is done, `RENDER_CHANNEL.send()` notifies the render task.

### 3.4 The Render Task: Work of the Consumer

The render task runs in its own Embassy task and loops waiting for render requests:

```rust
// src/display.rs:142-245 (core logic simplified, non-2.7-inch path)
loop {
    let render_sign = receiver.receive();           // wait for a render request
    let quickly_lut = quickly_lut.receive();        // or a refresh-mode switch

    match select(render_sign, quickly_lut).await {
        Either::First(render_info) => {
            // Mark "a refresh cycle is running" — sleep/power-off code must
            // wait for this to clear (see 3.7)
            RENDER_BUSY.store(true, Ordering::Relaxed);
            add_render_times();                     // render counter += 1

            // If the panel is asleep, wake it first (2.7" also rebuilds the base map)
            if is_sleep {
                epd.wake_up(&mut spi_device, &mut embassy_time::Delay);
                is_sleep = false;
            }

            let buffer = unsafe {
                (*core::ptr::addr_of_mut!(DISPLAY)).as_mut().unwrap().buffer()
            };

            // Decide whether a full refresh is needed (non-2.7-inch path)
            let need_clear = take_need_clear();     // true on cold-boot/button-wake first frame
            let need_force_full = (get_render_times() % get_full_refresh_times() == 0 || need_clear)
                && refresh_lut == RefreshLut::Quick;
            if need_force_full {
                spi_device = set_refresh_mode(RefreshLut::Full, &mut epd, spi_device);
            }

            // Send the framebuffer to the panel (issues MasterActivation and
            // returns without waiting for BUSY)
            epd.update_and_display_frame(&mut spi_device, buffer, &mut embassy_time::Delay)
                .expect("render failed");

            // Asynchronously wait for the physical refresh: poll BUSY every
            // 2ms, yielding to the executor in between
            wait_refresh_done(&mut epd).await;

            if need_force_full {
                // Switch back to the Quick LUT only after the refresh completes
                spi_device = set_refresh_mode(RefreshLut::Quick, &mut epd, spi_device);
            }

            // Put the panel to sleep if requested
            if render_info.need_sleep {
                is_sleep = true;
                epd.sleep(&mut spi_device, &mut embassy_time::Delay);
            }

            RENDER_BUSY.store(false, Ordering::Relaxed);
        },
        Either::Second(v) => {
            // Switch refresh mode (Quick = true / Full = false)
            RENDER_BUSY.store(true, Ordering::Relaxed);
            refresh_lut = if v { RefreshLut::Quick } else { RefreshLut::Full };
            spi_device = set_refresh_mode(refresh_lut, &mut epd, spi_device);
            RENDER_BUSY.store(false, Ordering::Relaxed);
        },
    }
    Timer::after(Duration::from_millis(50)).await;  // let things settle, loop
}
```

One key change here: **the refresh wait is asynchronous**. After the driver issues the refresh command it does not spin waiting for BUSY to drop; instead `wait_refresh_done()` polls every 2ms and yields to the executor with `Timer::after` each round — during the hundreds of milliseconds of a partial refresh, or the seconds of a full refresh, key scanning, next-page rasterization and other tasks all keep making progress instead of locking up the single-threaded executor.

### 3.5 Refresh Policy

The system keeps a render counter and forces a full refresh every N partial refreshes to clear ghosting. N's **platform default** is a compile-time constant (20 for the 2.7-inch panel, 5 for the rest), but it can be overridden at runtime from the settings page / web UI:

```rust
// src/display.rs:86-95
#[cfg(feature = "epd2in7")]
pub const DEFAULT_FULL_REFRESH_TIMES: u32 = 20;
#[cfg(not(feature = "epd2in7"))]
pub const DEFAULT_FULL_REFRESH_TIMES: u32 = 5;

// Runtime cache (0 = not loaded yet, fall back to the platform default)
static mut FULL_REFRESH_TIMES: u32 = 0;
```

`FULL_REFRESH_TIMES` is loaded from `DisplayStorage.full_refresh_times` in flash and clamped to `1..=100` by `reload_full_refresh_times()`. The clamp is mandatory: on firmware upgraded from old builds, that partition may still contain raw flash bytes (0 or a huge value), and the full-refresh decision involves division by N (N=0 would panic with a divide-by-zero). The settings page and the web UI call `reload_full_refresh_times()` immediately after a change.

```rust
// src/display.rs:100-106
pub fn reload_full_refresh_times() {
    let raw = crate::storage::DisplayStorage::read().map(|d| d.full_refresh_times).unwrap_or(0);
    let v = if (1..=100).contains(&raw) { raw } else { DEFAULT_FULL_REFRESH_TIMES };
    unsafe { core::ptr::addr_of_mut!(FULL_REFRESH_TIMES).write(v); }
}
```

The full-refresh decision itself differs per panel:
- **Non-2.7-inch**: `get_render_times() % get_full_refresh_times() == 0` (full refresh whenever the counter hits a multiple)
- **2.7-inch**: `get_render_times() >= get_full_refresh_times()`, followed by `reset_render_times()` after the full refresh (base-map semantics, see 3.6)

The render counter itself lives in RTC memory and survives deep-sleep wake-ups:

```rust
// src/display.rs:49-50
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES:u32 = 0;
```

There is also a cold-boot clearing flag, `NEED_CLEAR` (in rtc_fast, initialized to true, reset to false by `take_need_clear()` after the first render). It is set in two places:

1. Power-on cold boot: the rtc_fast section returns to its initial value of true;
2. Non-timer wake-ups (button/reset) entering the program: `main` explicitly calls `mark_need_clear()` at its wake-up classification point:

```rust
// src/main.rs:188-192
// Non-timer wake-up (button/power-on) entering the program: force a full
// refresh on the first frame to clear the previous session's ghosting;
// timer wake-ups keep quick refresh (a full flash on every minute-level
// wake-up would flicker and burn power).
if !timed_wake {
    crate::display::mark_need_clear();
}
```

Deep-sleep (timer) wake-ups never touch it, so minute-level timer wake-ups keep quick refresh (less flashing, less power); only a genuine cold boot or a user button press forces a full refresh on the first frame to clear ghosting left by the previous session.

### 3.6 Special Handling for the 2.7-Inch Panel

The 2.7-inch panel's partial refresh works differently — it needs a "previous frame" reference to compute the diff. But in deep sleep the panel controller's RAM is lost, so the first frame after a wake-up must re-establish the baseline.

```rust
// src/display.rs:60-65
#[cfg(feature = "epd2in7")]
static mut PREV_BUFFER: [u8; 5808] = [0xFF; 5808];  // backup of the previous frame
#[cfg(feature = "epd2in7")]
static mut PREV_BUFFER_VALID: bool = false;           // whether a valid previous frame exists
```

The three-branch rendering logic:

```rust
// src/display.rs:161-201 (simplified)
let render_count = get_render_times();
let need_force_full = render_count >= get_full_refresh_times() && refresh_lut == RefreshLut::Quick;

if refresh_lut == RefreshLut::Quick {
    if need_force_full {
        // Periodic full refresh to clear ghosting: set_base_map rebuilds the
        // base map and the counter resets to zero
        epd.set_base_map(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
        need_base_map = false;
        reset_render_times();
    } else if need_base_map {
        // After wake-up or first boot: quiet transition if a previous frame
        // exists, otherwise a full flash is unavoidable
        let prev_valid = unsafe { core::ptr::addr_of!(PREV_BUFFER_VALID).read() };
        if prev_valid {
            let prev = unsafe { &*core::ptr::addr_of!(PREV_BUFFER) };
            epd.set_base_map_quiet(&mut spi_device, prev, buffer, &mut embassy_time::Delay).expect("render failed");
        } else {
            epd.set_base_map(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
        }
        need_base_map = false;
    } else {
        // Normal partial refresh
        epd.update_and_display_frame_partial(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
    }
    // Save the current frame first, then wait for BUSY asynchronously — there
    // is no await between saving and uploading, so page tasks cannot modify
    // DISPLAY; during the BUSY wait pages may draw the next frame, but this
    // frame is already stored in PREV_BUFFER
    unsafe {
        (*core::ptr::addr_of_mut!(PREV_BUFFER)).copy_from_slice(buffer);
        core::ptr::addr_of_mut!(PREV_BUFFER_VALID).write(true);
    }
    wait_refresh_done(&mut epd).await;
}
```

This design is elegant: by keeping a backup of the previous frame in RAM, a "quiet" base-map set can be performed after a wake-up, avoiding the flicker of a full refresh. `need_base_map` is re-set both after a wake-up (following `wake_up`) and when the refresh mode is switched manually (a `QUICKLY_LUT_CHANNEL` message), guaranteeing the next render request re-establishes the baseline.

### 3.7 The Boundary of a Render Cycle: RENDER_BUSY and await_render_idle

Now that rendering is asynchronous, the executor schedules other tasks during a refresh — which means sleep/power-off code may run exactly while the panel is mid-refresh. Cutting EINK power or entering deep sleep at that moment leaves the panel in a half-driven state, producing the grayish "sleep ghosting" that only a full refresh can clear.

The render task therefore brackets each cycle with `RENDER_BUSY` (an AtomicBool) and exposes a wait function:

```rust
// src/display.rs:249-261
pub async fn await_render_idle() {
    loop {
        let busy = RENDER_BUSY.load(Ordering::Relaxed);
        let pending = !RENDER_CHANNEL.is_empty();
        if !busy && !pending {
            break;
        }
        Timer::after(Duration::from_millis(2)).await;
    }
}
```

Before every deep sleep (`to_sleep_tips` / `reboot_sleep`), the system first runs `await_render_idle().await` — waiting both for the current cycle to finish and for queued render requests to drain — before cutting the power rails and writing the sleep registers.

## 4. The Framebuffer and embedded-graphics

### 4.1 The Framebuffer

An e-paper framebuffer is a byte array where each bit represents one pixel (0 = white, 1 = black). Taking the 2.9-inch panel as an example:

```
296 × 128 pixels
= 296 × 128 / 8 bytes
= 4736 bytes (about 4.6KB)
```

The 4.2-inch framebuffer is larger: 400 × 300 / 8 = 15000 bytes (about 14.6KB). On an ESP32-C3 with 400KB of SRAM, that is a significant chunk of memory.

### 4.2 Drawing with embedded-graphics

This project uses the `embedded-graphics` crate as its drawing framework. Its core is the `DrawTarget` trait:

```rust
// trait defined by embedded-graphics
pub trait DrawTarget {
    type Color;
    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where I: IntoIterator<Item = Pixel<Self::Color>>;
}
```

`EpdDisplay` implements `DrawTarget`, so all of `embedded-graphics`' drawing capabilities are available:

```rust
// Text
let style = MonoTextStyleBuilder::new()
    .font(&FONT_6X10)
    .text_color(White)
    .background_color(Black)
    .build();
Text::new("Hello", Point::new(10, 20), style).draw(display);

// Rectangles
Rectangle::new(Point::new(0, 0), Size::new(100, 50))
    .into_styled(PrimitiveStyle::with_fill(Black))
    .draw(display);

// Custom widgets
calendar.draw(display);  // any component implementing the Drawable trait
```

### 4.3 Chinese Text Rendering

Chinese display is a core requirement of this project. The `u8g2-fonts` crate loads WenQuanYi bitmap fonts encoded for GB2312 (built-in wqy12 / wqy14 / wqy16 heights, chosen per panel size and use case):

```rust
// src/main.rs:270-271
let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>();
let _font = font.with_ignore_unknown_chars(true);  // skip characters that cannot be rendered
```

`u8g2_font_wqy14_t_gb2312` is the WenQuanYi font covering the full GB2312 Chinese character set. 14 is the glyph height in pixels — on a 296×128 panel that is roughly 21 characters per line and 7 lines of text.

Usage:

```rust
// src/display.rs:366-396 (show_sleep, fallback screen when no wallpaper exists)
let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>();
let font = font.with_ignore_unknown_chars(true);
let _ = font.render_aligned(
    "睡眠中",
    Point::new(
        display.bounding_box().size.width as i32 / 2,
        display.bounding_box().size.height as i32 / 2,
    ),
    VerticalPosition::Center,
    HorizontalAlignment::Center,
    FontColor::Transparent(Black),
    display,
);
```

`render_aligned` supports multiple horizontal and vertical alignment modes, which fits UI layout nicely.

## 5. The Standby Wallpaper System

The standby wallpaper is a clever application of e-paper — because the image survives power loss, the standby screen doubles as the device's "face".

The sleep picture has two entry points: normal mode goes through `display::show_sleep()`; reading mode registers its own render hook via `set_sleep_renderer()` (the `SLEEP_RENDERER` function pointer, which draws the reading progress plus a battery status line). When a hook is installed it takes priority; otherwise the wallpaper path runs.

### 5.1 Storage Location

The wallpaper is stored in the flash `storage` partition (starting at `0x310000`, 128KB):

```rust
// src/flash_sleep.rs:9-18
const STORAGE_OFFSET: u32 = 0x310000;
const SLEEP_IMAGE_MAGIC: u32 = 0x534C4550;
const HEADER_SIZE: usize = 8;

const SLEEP_IMG_W: u32 = 300;
const SLEEP_IMG_H: u32 = 400;
const ROW_BYTES: usize = (SLEEP_IMG_W as usize + 7) / 8;

const DEFAULT_SLEEP_PIXELS: &[u8; ROW_BYTES * SLEEP_IMG_H as usize] =
    include_bytes!("../files/sleep_default.bin");
```

Storage format: an 8-byte header plus pixel data. The header holds a magic number (`0x534C4550`, i.e. "SLEP") and the pixel data length. When flash contains no valid wallpaper, the built-in default wallpaper `files/sleep_default.bin` is used instead.

### 5.2 BMP Conversion and Scaling

A user-uploaded BMP must be converted to the panel's 1-bit format and scaled to the target size:

```rust
// src/flash_sleep.rs:38-109 (core logic simplified)
pub fn bmp_to_pixels(raw_bmp: &[u8], target_w: u32, target_h: u32) -> Option<AllocVec<u8>> {
    // Parse the BMP header (supports negative height / top-down rows)
    let bpp = ...; // supports 1-bit, 24-bit, 32-bit

    // Per-pixel scaling
    for dy in 0..target_h {
        let src_y = dy * bmp_h / target_h;  // nearest-neighbor scaling
        for dx in 0..target_w {
            let sx = dx * bmp_w / target_w;
            let is_black = match bpp {
                1 => /* read 1-bit pixel (0 = black) */,
                24 | 32 => {
                    // RGB -> grayscale -> binarize
                    let (b, g, r) = ...;
                    (r * 299 + g * 587 + b * 114) / 1000 < 128  // ITU-R BT.601
                },
            };
            // Pack into 1-bit (MSB first)
            if is_black {
                pixels[row_offset + idx / 8] |= 1 << (7 - (idx % 8));
            }
        }
    }
}
```

Grayscale-to-binary conversion uses the ITU-R BT.601 luminance formula: `Y = 0.299R + 0.587G + 0.114B` with a threshold of 128.

### 5.3 Streaming Read and Render

A wallpaper image can be large (300×400/8 ≈ 15000 bytes). To avoid one big allocation, rendering reads it row by row from flash:

```rust
// src/flash_sleep.rs:219-246
pub fn draw_sleep_image<D>(display: &mut D) -> bool
where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut header = [0u8; 8];
    let has_flash_image = read_flash(STORAGE_OFFSET, &mut header).is_ok()
        && u32::from_le_bytes([header[0], header[1], header[2], header[3]]) == SLEEP_IMAGE_MAGIC;

    if has_flash_image {
        let mut row_buf = [0u8; ROW_BYTES];
        for y in 0..SLEEP_IMG_H {
            let offset = STORAGE_OFFSET + HEADER_SIZE as u32 + y as u32 * ROW_BYTES as u32;
            if read_flash(offset, &mut row_buf).is_err() {
                return false;
            }
            // draw pixel by pixel into the framebuffer
            for x in 0..SLEEP_IMG_W {
                if (row_buf[x / 8] >> (7 - x % 8)) & 1 != 0 {
                    Pixel(Point::new(x as i32, y as i32), BinaryColor::On).draw(display);
                }
            }
        }
    } else {
        draw_pixels(display, DEFAULT_SLEEP_PIXELS, SLEEP_IMG_W, SLEEP_IMG_H);
    }
    true
}
```

Only one row (38 bytes) is read at a time, so memory usage is minimal.

### 5.4 Crash-Safe Writing

Wallpaper writes follow a three-step strategy: "clear the header, write the data, then write the valid header":

```rust
// src/flash_sleep.rs:195-217
pub fn save_sleep_image(raw_bmp: &[u8]) -> Result<(), &'static str> {
    let pixels = bmp_to_pixels(raw_bmp, SLEEP_IMG_W, SLEEP_IMG_H).ok_or("invalid BMP")?;

    // Step 1: clear the header (mark as invalid)
    write_flash(STORAGE_OFFSET, &zero_header)?;

    // Step 2: write the pixel data
    write_flash(STORAGE_OFFSET + HEADER_SIZE as u32, &pixels)?;

    // Step 3: write the valid header (mark as valid)
    header[0..4].copy_from_slice(&SLEEP_IMAGE_MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&(total_size as u32).to_le_bytes());
    write_flash(STORAGE_OFFSET, &header)?;

    Ok(())
}
```

If power is lost while the pixel data is being written, the header is invalid after reboot (all zeros) and the system falls back to the default wallpaper. The header is marked valid only after a complete write. A simple but effective crash-safety policy. Large web uploads use the same strategy in stepped form (`begin_sleep_image_write` → per-row `write_sleep_pixel_row` → `finish_sleep_image_write`), avoiding holding the whole image buffer at once.

## 6. The Rendering Pipeline, Summarized

The complete rendering pipeline:

```
User action / timer
      │
Event dispatch (dispatch task → page event callback)
      │
Page state changes
      │
display_mut() → grab the framebuffer (static mut)
      │
embedded-graphics API drawing
(Text / Rectangle / Widget / Font)
      │
RENDER_CHANNEL.send(RenderInfo)
      │ (async Channel, non-blocking)
      │
render task receives the request, RENDER_BUSY=true
      │
Is the panel asleep? → wake_up()
      │
Refresh counter / NEED_CLEAR → full refresh needed?
      │
epd.update_and_display_frame(buffer)
      │ (sent to the panel over SPI)
      │
wait_refresh_done() — poll BUSY every 2ms,
yielding the executor in between
      │
need_sleep? → put the panel to sleep
      │
RENDER_BUSY=false → Timer::after(50ms), loop
```

The sleep path additionally calls `await_render_idle()` before cutting power — waiting for RENDER_BUSY to clear and the queue to drain — so the panel is never left mid-refresh.

## Summary

| Topic | Key Points |
|-------|------------|
| Panel driver | epd-waveshare (custom fork) + conditional compilation for multiple sizes; panel/board features guarded at compile time |
| Rendering architecture | Producer-consumer pattern; a Channel decouples pages from hardware |
| Async refresh | The driver issues the command and waits for BUSY asynchronously (wait_refresh_done, 2ms polling that yields); await_render_idle drains before sleep/power-off |
| Refresh policy | Force a full refresh every N quick refreshes; N runtime-configurable (DisplayStorage, clamped to 1..=100), counter kept in RTC; first frame after cold boot/button wake forces a full refresh (NEED_CLEAR) |
| 2.7-inch special handling | PREV_BUFFER frame backup, quiet transition after wake-up; counter-based full refresh rebuilds the base map via set_base_map and resets |
| Framebuffer | 1-bit bitmap accessed globally through core::ptr |
| Drawing | embedded-graphics DrawTarget + u8g2 WenQuanYi bitmap Chinese fonts (wqy12/14/16) |
| Standby wallpaper | Flash storage + BMP conversion + streaming render + crash-safe write; reading mode can register a custom sleep render hook |

The core design idea: **wrap slow hardware operations in a dedicated task and decouple them through an async Channel, so page code can focus on business logic instead of hardware details**.

In the next article we dive into the Embassy async runtime and the event-driven architecture, watching how a key press travels from a GPIO level change to a page response.

---

> Previous: [Part 2: Hardware Abstraction and SPI Bus Sharing](../02-硬件抽象与SPI总线共享.md) · Next: [Part 4: Embassy Async Runtime and Event-Driven Architecture](04-embassy-async-and-event-driven.md)
