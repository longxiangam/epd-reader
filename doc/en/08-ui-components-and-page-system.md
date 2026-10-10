# Part 8: UI Components and the Page System

> 中文版: [../08-UI组件化与页面系统.md](../08-UI组件化与页面系统.md)

> Building an ESP32-C3 E-Ink Reader in Rust · Article Series

## Introduction

Designing a UI on a 296×128 or 400×300 pixel e-ink display is a unique experience. No color, no animation, slow refresh — but you control the position of every element with pixel precision. This project's UI system is built on the `embedded-graphics` framework, and achieves a clean application structure through componentization and page routing.

## 1. embedded-graphics and the Drawable Trait

`embedded-graphics` is the cornerstone of Rust embedded graphics. Its core is the `Drawable` trait:

```rust
// trait defined by embedded-graphics
pub trait Drawable {
    type Color: PixelColor;
    type Output;
    fn draw<D: DrawTarget<Color = Self::Color>>(
        &self,
        target: &mut D,
    ) -> Result<Self::Output, D::Error>;
}
```

Any type implementing `Drawable` can be drawn onto any `DrawTarget` — the screen, a framebuffer, even another composite component. This decoupling means a component never needs to know where it will ultimately be drawn.

Every UI component in this project implements `Drawable`:

```rust
// src/widgets/icon_grid_widget.rs:132-148
impl<C> Drawable for IconGridWidget<C>
where C: PixelColor + Clone
{
    type Color = C;
    type Output = ();

    fn draw<D>(&self, target: &mut D) -> Result<Self::Output, D::Error>
    where D: DrawTarget<Color = Self::Color>
    {
        for cell in self.cells.iter() {
            let _ = cell.draw(target);  // each cell is a Drawable too
        }
        Ok(())
    }
}
```

## 2. The Icon Grid Menu (IconGridWidget)

The main menu uses an icon grid layout; each cell is a geometric icon plus a text label:

```rust
// src/widgets/icon_grid_widget.rs:15-24
pub struct IconGridWidget<C> {
    position: Point,
    size: Size,
    columns: usize,           // column count (3 in this project)
    front_color: C,
    back_color: C,
    choose_index: usize,      // currently selected item
    cells: Vec<GridCell<C>, 20>,
}
```

### Automatic Layout

The grid computes each cell's size from the column count and the number of items:

```rust
// src/widgets/icon_grid_widget.rs:85-86
let cell_width = size.width / columns as u32;
let cell_height = size.height / ((items.len() + columns - 1) / columns) as u32;
```

On the 4.2-inch screen (400×300) with 3 columns, each cell is roughly 133×100 pixels. The selected cell is highlighted with a 2px rounded-rectangle border (unselected cells get a 1px thin border).

### Purely Geometric Icons

Because an e-ink display has only black and white, colored icons are not an option. The project draws all 7 icons (book/weather/calendar/settings/image/debug/stock) using `embedded-graphics` primitives (lines, rectangles, circles):

```rust
// src/widgets/icon_grid_widget.rs:219-234 (book icon)
IconType::Book => {
    // open book: two slanted lines forming the spine
    let spine_top = Point::new(center.x, center.y - half);
    let left_top = Point::new(center.x - half, center.y - q);
    // ... 6 lines forming the outline of a book
    Line::new(spine_top, left_top).into_styled(style).draw(target)?;
    // ... inner horizontal lines for the pages
}

IconType::Weather => {
    // sun: circle + 8 rays
    Circle::new(...).into_styled(style).draw(target)?;
    for (dx, dy) in dirs { /* rays in 8 directions */ }
}

IconType::Stock => {
    // candlestick: hollow rectangle + wick for up, solid for down
}

IconType::Settings => {
    // gear: outer circle + 6 teeth + inner circle
}
```

These purely geometric icons work very well on e-ink — crisp lines and high contrast.

## 3. Temperature Trend Chart (TempChart)

The temperature curve on the weather page is a typical data-visualization component:

```rust
// src/widgets/temp_chart.rs:23-108 (simplified)
pub fn draw_temp_chart<C, D>(
    position: Point,
    chart_size: Size,
    points: &[TempPoint],    // per-day temperature data
    color: C,
    target: &mut D,
) -> Result<(), D::Error>
{
    // 1. Compute the temperature range
    let temp_min = ... - 2;  // margin
    let temp_max = ... + 2;

    // 2. Y-coordinate mapping function
    let temp_to_y = |temp: i32| -> i32 {
        chart_y + chart_h - ((temp - temp_min) * chart_h / temp_range)
    };

    // 3. Horizontal reference lines (one every 5°C)
    let mut t = (temp_min / 5) * 5 + 5;
    while t < temp_max {
        Line::new(...).into_styled(stroke).draw(target)?;
        t += 5;
    }

    // 4. High-temperature polyline + low-temperature polyline
    draw_polyline(&high_pts, thick_stroke, dot_fill, target)?;
    draw_polyline(&low_pts, thick_stroke, dot_fill, target)?;
}
```

The polylines are drawn as a combination of points and line segments:

```rust
// src/widgets/temp_chart.rs (simplified)
fn draw_polyline<C, D>(pts: &[Point], line_style, dot_style, target) {
    // connect the dots
    for i in 1..pts.len() {
        Line::new(pts[i - 1], pts[i]).into_styled(line_style).draw(target)?;
    }
    // data points (small circles)
    for &p in pts {
        Circle::new(Point::new(p.x - 2, p.y - 2), 4)
            .into_styled(dot_style).draw(target)?;
    }
}
```

## 4. The Calendar Component (Calendar)

The calendar is the most complex UI component, integrating three layers of data: solar calendar, lunar calendar, and holidays:

```rust
// src/widgets/calendar.rs:18-26
pub struct Calendar<C> {
    pub position: Point,
    pub size: Size,
    month_first_day: Date,
    month_last_day: Date,
    today: Date,
    front_color: C,
    back_color: C,
}
```

The calendar implements the `Drawable` trait and uses `embedded-graphics` clipping areas to bound its drawing:

```rust
// src/widgets/calendar.rs:68-80
impl<C> Drawable for Calendar<C> where C: PixelColor {
    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error> {
        let clipping_area = Rectangle::new(self.position, self.size);
        let mut display = display.clipped(&clipping_area);  // clip
        // ... draw on the clipped display
    }
}
```

## 5. The Page Trait and Page Routing

### 5.1 The Page Trait

Every page implements a unified `Page` trait:

```rust
// src/pages/mod.rs:57-77
pub trait Page {
    fn new() -> Self;
    async fn render(&mut self);      // render the page content
    async fn run(&mut self, spawner: Spawner);  // run the page logic
    async fn bind_event(&mut self);  // bind event listeners

    // Raw-pointer helpers: event callbacks need &mut to the page, but callbacks
    // are 'static — the page's address is stored in the listener at bind time
    // and converted back inside the callback
    fn mut_by_ptr<'a, T>(ptr: Option<usize>) -> Option<&'a mut T> { ... }
    fn mut_to_ptr<T>(ref_mut: &mut T) -> usize { ... }
}
```

Event binding uses `on_target(event, ptr, callback)` to register the page's address along with the callback (see 5.3); inside the callback, `mut_by_ptr` converts it back to `&mut Self`. This is the idiomatic way to let a callback hold page state in a no_std environment; the cost is that the pointer dangles once the page is moved or dropped, so leaving a page must `clear()`.

### 5.2 Page Routing

`MainPage` is the page router. It maintains the menu list and the currently selected page:

```rust
// src/pages/main_page.rs:272-355 (simplified)
async fn run(&mut self, spawner: Spawner) {
    loop {
        if None == self.current_page {
            self.render().await;  // render the main menu (IconGridWidget, 3 columns, 7 items)
            continue;
        }
        let menu_item = &self.menus[self.current_page.unwrap()];
        match menu_item.page_enum {
            EReadPage => {
                // Restart-based mode: set the rtc_fast reading flag + NVS sentinel
                // → after the 50 ms short deep sleep, main enters reading mode directly (see Part 7, 1.7)
                unsafe {
                    core::ptr::addr_of_mut!(crate::pages::read::READING_MODE).write(true);
                    core::ptr::addr_of_mut!(crate::pages::read::PAGE_INDEX).write(None);
                }
                crate::storage::write_sleep_page(
                    crate::storage::SleepPageStorage::PAGE_READING,
                );
                crate::sleep::reboot_sleep().await;
            }
            EWeatherPage => {
                let mut weather_page = WeatherPage::new();
                weather_page.bind_event().await;
                weather_page.run(spawner).await;
                self.back().await;  // rebind the main-menu events on return
            }
            ECalendarPage | EStockPage | EImageListPage | ESettingPage | EDebugPage => { /* same */ }
            // ...
        }
    }
}
```

The main menu has 7 items: e-book, weather, calendar, stock, images, settings, debug. The router also handles "restoring the scene":

- A cold boot enters the configured default home page (`read_default_page()`, stored in NVS, changeable in the settings page: e-book/weather/calendar/stock/main menu);
- A deep-sleep wake returns to the last page (rtc_fast `PAGE_INDEX`, restored from the NVS sentinel when an IO wake loses it);
- The focus stays consistent with the auto-opened app: after returning to the main menu, the highlight rests on that app's icon rather than jumping back to item 0.

Page lifecycle:

```
MainPage.run()
  │
  ├── current_page == None → render the main menu
  │
  └── current_page == Some(index)
        │
        create the subpage → bind_event() → run() → return
        │
        back() → clear() + rebind the main-menu events
```

### 5.3 Event Binding and Cleanup

Each time a subpage is entered, the previous page's listeners are cleared first, then the new ones bound:

```rust
// src/pages/setting_page.rs (typical pattern, consistent across pages)
async fn bind_event(&mut self) {
    event::clear().await;  // clear all non-resident listeners
    event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), |info| { ... }).await;
    event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), |info| { ... }).await;
    event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), |info| { ... }).await;
}
```

`clear()` only removes listeners with `fixed: false`; resident listeners registered via `on_fixed` survive page switches. Events themselves go through a queue-based dispatch: the key-scanning task (`event::run`) only produces events and enqueues them (capacity 8, oldest dropped when full), and the `dispatch` task delivers them serially to listener callbacks — keys are not lost even while a long callback runs. During dispatch the listener-table lock is held and callbacks run serially, so callbacks must not rebind via `on/clear` (it would deadlock); rebinding is done by the page's `run` loop outside of dispatch.

### 5.4 The Stock Page: Two Views and Trading-Session Awareness

The stock page (`pages/stock`) is a data-heavy page, and several of its design decisions deserve a closer look.

**Overview + detail views** (`STOCK_VIEW` in rtc_fast, restored across deep sleep): the overview is a grid of 6 stocks, each cell showing the "previous day + today" intraday trend (split screen, vertical dashed divider); confirming a cell enters that stock's detail view. Overview data comes from the rtc_fast `STOCK_MINUTE_CACHE` (6 × 96 close-price slots + previous close, about 1.3KB) — a deep-sleep wake restarts the program and DRAM is lost, but this cache survives, so re-entering the overview within 30 seconds renders straight from cache without re-fetching. Leaving the page resets the view to the overview.

**Six chart modes in a ring** (`model/stock.rs::ChartMode`): minute → day K → week K → month K → line → quote panel → minute. "Day K" and "line" share one data source (`scale=240`), so switching between them does not re-fetch — only the rendering changes; other cross-source switches do re-fetch. The current mode is encoded as a `u8` in rtc_fast (`STOCK_MODE`) and restored after a deep-sleep wake, so the minute mode's "wake every 2 minutes" period is not reset.

**Fetches moved out of key callbacks**: key callbacks only set flags (`need_fetch` / `need_clean_render`); the actual network requests run in the `run()` loop — keys stay responsive while a fetch is in progress. Switching stocks/modes does **not** clear the old data: the previous chart stays on screen during the fetch (like the weather page), and is replaced when the new data arrives.

**Two data sources + two parsers**: K-lines come from Sina's `CN_MarketData.getKLineData` (plain HTTP), the realtime quote panel from Tencent's `qt.gtimg.cn`. Sina returns JSON, but `parse_kline` **deliberately avoids mini_json** — 60 rows × 6 fields would make mini_json allocate 12~15KB on the heap (OOM within a 64KB heap), so a zero-allocation byte scanner fills a `heapless::Vec` directly. Tencent's response is GBK-encoded and `~`-separated; `split_gbk_tilde` skips the trail byte after a GBK lead byte so a GBK trail byte of `0x7E` is not mistaken for a separator.

**The refresh period follows the trading session** (holiday-aware):

```rust
// src/pages/stock/page.rs:591-606 (simplified)
let realtime = self.view == StockView::Overview || self.mode.is_realtime();
let sleep_secs: u64 = if !realtime {
    12 * 3600                              // day/week/month K: 12 hours
} else {
    match crate::worldtime::get_clock() {
        Some(clock) => {
            let now = clock.local().await;
            if market_open_now(&now, holidays.as_ref()) {
                120                        // market open: wake and refresh every 2 minutes
            } else {
                secs_until_next_open(&now, holidays.as_ref())  // sleep until the next open
            }
        }
        None => 120,                       // no clock source to judge with; keep the old rate
    }
};
```

`market_open_now` checks trading days 9:15~11:31 / 12:55~15:01 (Mon~Fri, corrected by holiday-cn for closures and make-up workdays when synced); `secs_until_next_open` computes the seconds until the next open directly during closed hours — lunch break sleeps until 12:55, and after close/weekends/holidays until 9:25 on the next trading day, with no idle refreshing in between. The idle sleep threshold reuses the configured "weather sleep time".

Key bindings are split by view: overview (short 1/2 move the selection, long 1 force-refreshes, short 3 enters the detail, long 3 returns to the main menu); detail (short 1/2 switch chart modes, long 1/2 switch stocks, short 3 re-fetches, long 3 returns to the overview).

For rendering, candlesticks/lines are drawn by `widgets/kline.rs` (close ≥ open draws hollow, close < open solid; when the screen cannot fit them all, only the most recent bars are drawn, keeping each bar ≥7px). The quote panel uses a table layout for the basic info and the five-level bid/ask book.

### 5.5 The Reading Page: Menu, Bookmarks, and the Book List

The reading page (`pages/read/page.rs`) is the project's largest page (nearly 3000 lines). It runs in a separate "restart-based mode" (see Part 7, 1.7): no WiFi init, and the whole heap goes to TTF rendering. At the UI level there are four state machines worth expanding on.

**The 9-item reading menu** (`MENU_ITEMS`): back to book list / bookmarks / jump to progress / typesetting / font / rotate screen / swap direction keys / sleep / cancel. The menu is an overlay panel drawn on top of the text. All interaction states converge into one enum:

```rust
// src/pages/read/page.rs:117-128
enum MenuState {
    Closed,
    Popup { menu_index: u32 },                    // main menu
    JumpInput { input_pct: u8 },                  // jump to progress (percentage input)
    /// Typesetting: cursor 0=font size 1=line gap 2=stroke 3=reset 4=back;
    /// editing=true means up/down change the value
    Layout { cursor: u8, editing: bool },
    FontList { font_index: u32 },                 // font list (SD TTFs + 3 built-in bitmap fonts)
    /// Bookmark list: bm_index 0=bookmark-current 1..=n=bookmarks n+1=cancel
    BookmarkList { bm_index: u32 },
    /// Bookmark action popup: action 0=open 1=delete
    BookmarkAction { bm_index: u32, action: u32 },
}
```

Rendering is incrementally optimized accordingly: `menu_state_id()` assigns each state a number; only a cross-state switch (different panel geometry) or an actual text change (rotation/font size/line gap) clears the screen and redraws the text; moving the cursor within a state only redraws the overlay — skipping the SD read + TTF rasterization makes menu operation noticeably smoother. The overlay panels come with a white fill, so redrawing erases the previous frame's content by itself.

**Bookmarks**: a bookmark list state plus an action popup, two levels. The first list row is fixed as "bookmark current position" (confirming writes the current progress into .log), followed by the existing bookmarks, and a cancel row at the end; confirming a bookmark opens a two-row "open/delete" popup. The selected row also loads a preview — 512 bytes are read from the SD at the bookmark offset, with incomplete UTF-8 sequences at both ends trimmed before display (512 % 3 = 2, so Chinese text almost certainly lands mid-character; without boundary trimming, `from_utf8` fails for the whole chunk). Opening a bookmark jumps to the recorded byte offset (bit31 is the "reached last page" marker; the low 31 bits are the offset).

**Two-level book list**: the `books/` root lists subdirectories and books; inside a subdirectory the first row becomes ".. back to parent". Both the directory level and the book index live in rtc_fast (`LIST_SUBDIR`/`LIST_BOOK_IDX`): the next ordinary entry into the book list restores the directory of the last opened book and selects it; resume reading uses `RESUME_SUBDIR` to restore the directory instead. Level changes use `change_dir` to switch directories in place (the old handle closes automatically); going back to the parent reopens `books/` via the SD root to release the directory slot.

**Direction-key swap**: within the reading functions (page turns/menu/jump/typesetting/font/bookmarks/book list), Key1/Key2 directions are swapped wholesale. The implementation is extremely thin — `bind_event` swaps the two physical keys' `down` value according to `swap_keys` and hands it to the unified `dir_key_short/dir_key_long`:

```rust
// src/pages/read/page.rs:2949-2957
// Key1 short: down / +1 / next page / next book (with "swap direction keys" on → up)
event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
    Box::pin(async move {
        let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
        let down = !mut_ref.swap_keys;      // swap on → Key1 becomes "up"
        mut_ref.dir_key_short(down).await;
    })
}).await;
```

The toggle is stored in flash (`ReadingFlagsStorage`, magic "PGSW") and survives restarts. The typesetting menu (font size/line gap/stroke bolding/reset/back) works the same way: in edit mode up/down change the value with long-press auto-repeat, and confirming or leaving packs `ttf_px/line_gap/rotate/font_name` into the flash `ReadingSettings`.

### 5.6 The Settings Page: List Menu and Edit Mode

The settings page (`pages/setting_page.rs`) uses a `ListWidget` for 11 items and has three modes (`SettingMode`): QR code (web provisioning), settings list, and auto-location. List items: reading sleep / weather sleep / auto-location / web config / stock / default home / full-refresh count / network time sync / update holidays / time display / back.

The interaction is uniformly "select → confirm to enter edit mode (the item shows `>>value<<`) → up/down to change → confirm to save": sleep times step by 5 seconds (reading minimum 10 seconds), the stock cycles within the configured list, the default home cycles through five values, and the full-refresh count spans 1~100 (saving calls `reload_full_refresh_times` to take effect immediately). "Network time sync" triggers one manual NTP sync + write to the PCF8563, and "update holidays" re-fetches holiday-cn; both show their last successful timestamp on the item (persisted in NVS). The web service task runs only on the QR-code screen and stops when you navigate away.

The page's run loop calls `reset_render_times()` every round — the settings screen always uses fast updates, preventing a sudden flicker from the full-refresh threshold; long-press 1/2 auto-repeats list navigation (400ms throttle); and on the QR screen, holding the left key for 10 seconds resets the device (wipes NVS + soft reset).

### 5.7 The Weather and Calendar Pages: Standby Rendering and Minute Alignment

The weather/calendar pages share a "low-density standby render": on entry each registers a compact layout function via `set_sleep_renderer(Some(sleep_renderer))`, and the system's `show_sleep()` calls it to draw the standby frame before sleeping (instead of the full page), keeping the sleep screen clean. The run loop also waits for the first AHT20 indoor temperature/humidity sample (v2 board I2C sensor, normally ~300ms, 2s cap) before rendering the first frame — so the timed wake's "sleep as soon as rendered" never sleeps on a frame missing data. The timed-wake period is computed as "time display on → align to the next minute boundary, off → sleep until the weather data expires" (see Part 7, 5.3).

## 6. Compile-Time Adaptation to Different Screens

Screens of different sizes need different UI parameters. Conditional compilation decides at build time:

```rust
// src/epd2in9_txt.rs:5-24
#[cfg(feature = "epd2in9")]
const LINES_NUM: u32 = 7;      // 2.9 inch: 7 lines of text
#[cfg(feature = "epd2in9")]
pub const WIDTH: u32 = 296;
#[cfg(feature = "epd2in9")]
pub const HEIGHT: u32 = 128;

#[cfg(feature = "epd4in2")]
const LINES_NUM: u32 = 22;     // 4.2 inch: 22 lines of text
#[cfg(feature = "epd4in2")]
pub const WIDTH: u32 = 300;
#[cfg(feature = "epd4in2")]
pub const HEIGHT: u32 = 400;

#[cfg(feature = "epd2in7")]
const LINES_NUM: u32 = 9;      // 2.7 inch: 9 lines of text
#[cfg(feature = "epd2in7")]
pub const WIDTH: u32 = 264;
#[cfg(feature = "epd2in7")]
pub const HEIGHT: u32 = 176;
```

Screen type (epd2in9/epd2in7/epd4in2) and board type (board_v1/board_v2) are each pick-one, guarded by `compile_error!` in `board/mod.rs`; one device = one epd + board combination fixed at compile time. This compile-time adaptation has zero runtime cost — unselected code is never compiled at all.

## 7. Component Inventory

| Component | File | Function | Technical points |
|------|------|------|---------|
| IconGridWidget | icon_grid_widget.rs | icon grid menu | automatic layout, geometric icons (7 kinds, incl. stock) |
| ListWidget | list_widget.rs | scrollable list | virtual scrolling (selection follows half-screen offset), selection highlight, 20-item capacity |
| Calendar | calendar.rs | calendar grid | solar+lunar+solar terms+holidays, clipping area |
| WeatherIcon | weather_icon.rs | weather icons | ~20 kinds of weather (19 classes + unknown), 32×32 BMP |
| TempChart | temp_chart.rs | temperature polylines | high/low dual polylines, reference lines, data points |
| KLine | kline.rs | stock candles/line | hollow/solid by up/down, window truncation, price→Y mapping |
| QrcodeWidget | qrcode_widget.rs | QR code | generated by qrcodegen-no-heap |
| Battery | battery.rs | battery icon | percentage + icon |
| draw_icon | draw_icon.rs | status icons | wifi / loading / moon (sleep) |
| ScrollBar | scroll_bar.rs | scroll bar | horizontal/vertical (used internally by ListWidget) |

## Summary

| Technique | Key point |
|--------|------|
| Drawable trait | components decoupled from draw targets, reusable |
| Geometric icons | no image assets, well suited to e-ink |
| Page trait | unified page interface: new/render/run/bind_event + raw-pointer round-trip |
| Page routing | MainPage as the router, match-dispatching to subpages; reading uses the restart-based mode |
| Event binding | clear + on_target on page entry; queue-based dispatch keeps keys from being lost during long callbacks |
| Overlay menus | MenuState state machine + state-id dedup; cursor moves only redraw the overlay |
| Compile-time adaptation | build-time selection of three screens + two board generations |
| Clipping area | embedded-graphics clipping bounds the drawing region |

At very low resolutions, the core of UI design is **information density and readability** — every pixel must earn its place. Componentization lets different pages reuse the same UI building blocks, and the Page trait guarantees uniform lifecycle management.

In the next article, we will discuss error handling and debugging in embedded environments.

---

> Previous: [Part 7: Low-Power Design and Power Management](07-low-power-and-power-management.md) · Next: [Part 9: Error Handling and Debugging](../09-错误处理与调试.md)
