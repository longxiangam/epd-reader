# Part 10: E-book Reading — Vector Fonts / Bitmap Fonts / Dynamic Pagination

> 中文版: [../10-电子书阅读与矢量字体.md](../10-电子书阅读与矢量字体.md)

> Building an ESP32-C3 E-ink Reader in Rust · Article Series

E-book reading is the core feature of this device. It has to lay out TXT novels nicely on a 400×300 e-ink display, page through them fluidly, and support arbitrary font sizes, line-spacing/stroke tuning, bookmarks, jumps, resume-reading, font selection, screen rotation, key-swap, and a two-level book list — all on an ESP32-C3 with 400KB SRAM and no FPU. This article covers the whole reading subsystem, from glyph rendering at the bottom to navigation at the top.

## Why Reading Deserves Its Own Article

The reading subsystem has several unique problems you won't meet elsewhere:

1. **Arbitrary font size + Chinese**: menus can use a fixed bitmap font, but reading must let the user change the font size — a fixed font can't do that. We need a **vector font** (TTF), rasterized on demand per size.
2. **Memory pressure**: TTF parsing needs large buffers (point coordinates, glyph outlines, glyph bitmap cache). The ESP32-C3 only has 400KB SRAM, and in normal mode the WiFi stack alone eats a big chunk.
3. **Soft-float rasterization is slow**: the C3 has no FPU. TTF quadratic Bézier flattening + per-pixel coverage computation is heavy work; a page with hundreds of characters needs careful optimization to stay responsive.
4. **Pagination and bidirectional paging**: e-ink refresh is slow, so the paging algorithm defines the experience — we can neither pre-build a whole-book index (which would stall entering a book) nor skip backward paging.

This project's answer: **reading and normal mode boot separately** (no memory contention) + **dual font modes, TTF and bitmap** + **byte-offset dynamic pagination**. One by one below.

---

## 1. Reading Mode: Separate Reboot, Exclusive Heap

### Why Two Modes

Normal mode (weather/stocks/calendar/provisioning) needs WiFi; embassy-net + the TLS stack eat roughly 60KB of heap. Reading mode needs a big TTF cache plus a deep call stack. The two are **runtime-heap exclusive** — run both and you get OOM.

If the TTF buffers lived in `.bss` permanently, normal mode would permanently lose that memory; if they were `Box`-allocated during reading, the WiFi heap in normal mode would fall short. The cleanest solution is **mode-split boot**: entering reading reboots the device, WiFi is never initialized, and the whole heap goes to TTF; leaving reading reboots back to normal mode.

### Implementing the Mode-Split Reboot

The key is **`reboot_sleep()`** (50ms deep sleep + timer wake), not `software_reset()`:

```rust
// src/sleep.rs
pub async fn reboot_sleep() {
    // ... wait for render idle, power off EINK/SD rails ...
    RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());
}
```

A deep-sleep wake reruns `main`, which re-allocates the heap and re-decides whether to bring up WiFi, and **`rtc_fast` memory survives deep sleep** (but **not `software_reset`**). So the cross-reboot flags live in `rtc_fast`:

```rust
// src/pages/read/page.rs
#[ram(unstable(rtc_fast))]
pub(crate) static mut PAGE_INDEX: Option<u32> = None;   // selected book index
#[ram(unstable(rtc_fast))]
pub(crate) static mut READING_MODE: bool = false;        // boot into reading?
#[ram(unstable(rtc_fast))]
static mut TTF_RESUME_OFFSET: u32 = 0;                   // cross-reset resume offset
#[ram(unstable(rtc_fast))]
static mut RESUME_SUBDIR: [u8; BOOK_NAME_MAX] = ...;     // subdir of the book being resumed
#[ram(unstable(rtc_fast))]
static mut LIST_SUBDIR: [u8; BOOK_NAME_MAX] = ...;       // list memory: last subdir
#[ram(unstable(rtc_fast))]
static mut LIST_BOOK_IDX: u32 = 0;                       // list memory: last selected book index
```

### Boot Dispatch (src/main.rs)

`main` checks `READING_MODE` at the top: if set it spawns `reading_task` and skips WiFi; otherwise it proceeds into normal mode and connects. An RTCIO wake loses `rtc_fast`, so the reading flag has an **NVS fallback** too (the `SleepPageStorage` sentinel value 100 — see section 4):

```rust
// v22: an IO (RTCIO) wake loses rtc_fast's READING_MODE — restore "was reading" from NVS
if crate::storage::read_sleep_page() == Some(SleepPageStorage::PAGE_READING) {
    core::ptr::addr_of_mut!(crate::pages::read::READING_MODE).write(true);
}
let reading_mode = unsafe { *core::ptr::addr_of!(crate::pages::read::READING_MODE) };
if reading_mode {
    crate::worldtime::init_clock_pcf().await;   // no WiFi, restore clock from PCF8563
    spawner.spawn(crate::battery::test_bat_adc().unwrap());
    spawner.spawn(crate::pages::reading_task(spawner.clone()).unwrap());
    // main never returns; interaction is handled by reading_task
    loop { Timer::after(Duration::from_secs(5)).await; }
}
```

### Entering / Leaving Reading

- **Enter**: pick "E-book" on the main grid → the `EReadPage` branch sets `READING_MODE=true`, `PAGE_INDEX=None`, writes the `PAGE_READING` sentinel to NVS → `reboot_sleep()` → on wake `main` enters the book list. Selecting a book sets `PAGE_INDEX=Some(book)` and records the book's subdirectory into `RESUME_SUBDIR`.
- **Leave**: `ReadPage::run()` returns (`running=false`) → `reading_task` clears `READING_MODE=false`, sets the main-grid page `PAGE_INDEX=-1`, writes -1 to NVS → `reboot_sleep()` → back to normal mode.

```rust
// src/pages/mod.rs
#[embassy_executor::task]
pub async fn reading_task(spawner: Spawner) {
    // Wait for the display task (reading_task starts faster than EPD init;
    // without waiting the first frame is blank and unrotated)
    let mut waited = 0u32;
    while crate::display::display_mut().is_none() {
        if waited > 150 { break; }  // at most ~3s (defensive; normally ready in ~100ms)
        waited += 1;
        Timer::after(Duration::from_millis(20)).await;
    }
    let mut p = read::ReadPage::new();
    p.bind_event().await;
    p.run(spawner).await;
    // Leaving reading: clear flags → reboot_sleep back to normal mode
    unsafe {
        core::ptr::addr_of_mut!(read::READING_MODE).write(false);
        core::ptr::addr_of_mut!(main_page::PAGE_INDEX).write(-1);
    }
    crate::storage::write_sleep_page(-1);
    crate::sleep::reboot_sleep().await;
}
```

### Where the Heap Lives

The heap lives in the dedicated **`dram2` section** (~64KB of DRAM freed by the bootloader, previously idle), not in main DRAM:

```rust
// src/main.rs
#[link_section = ".dram2_uninit"]
static mut DRAM2_HEAP: MaybeUninit<[u8; 65280]> = MaybeUninit::uninit();
// esp_alloc::HEAP.add_region(HeapRegion::new(ptr, 65280, Internal.into()));
```

All of main DRAM is left to the stack (the TTF render async chain is deep; when the 64KB heap sat in main DRAM we had stack overflows). Reading and normal mode share this one heap — no per-mode split needed.

---

## 2. Two Font Modes

Reading supports two font kinds; the user switches between them in the "Font" menu:

| | TTF vector font | Bitmap font |
|---|---|---|
| Source | SD card `fonts/*.ttf` (bitmap only if the dir is missing) | Built-in u8g2 wqy bitmaps in firmware |
| Size | Arbitrary (user-adjustable 12–40px) | Fixed (12/14/16) |
| Width | Proportional per `advance` | Fixed cell (CJK full width / ASCII half) |
| Render | Soft-float rasterization (slow, ~10ms/glyph cold cache) | Direct bitmap blit (very fast) |
| Coverage | All glyphs in the TTF (simhei etc.) | GB2312 subset (~7700 chars) |
| Pagination | Per-character `advance` measurement | Pure arithmetic |

### 2.1 TTF Vector Font (src/ttf_sd.rs)

TTF rendering does **not** use a third-party parser; it is a hand-rolled on-demand parser. The reason: on embedded you want every buffer packed into one controllable workspace, reused as much as possible, with minimal SD reads.

**Parse flow** (table directory read on first `open_font`; the result is cached in `TtfWs.fi`):

```
table dir → head(units_per_em/loc_fmt) / hhea(ascent/descent/num_h_metrics)
          → maxp(num_glyphs) / cmap(char→glyph map) / loca(glyph offsets) / glyf(glyph data) / hmtx(horizontal metrics)
```

So that paging doesn't hit the SD per character, the **whole cmap/hmtx/loca subtables are cached in RAM** (`cmap_cache`/`hmtx_cache`/`loca_cache` in `TtfWs`, caps 28KB/1KB/1KB); only `glyf` (glyph outlines, the largest) is read per glyph via random `seek+read` from the SD:

```rust
// Per glyph: char_to_gid(cmap) → loca offset → read glyf → parse outline → rasterize
let gid = char_to_gid(font_f, ws, fi, ch);   // cmap hits RAM cache, else SD fallback
let adv = advance(font_f, ws, fi, gid);       // hmtx hits RAM cache
let (pts, contours, bbox) = parse_glyph(font_f, fi, gid, ws);  // read glyf
let bmp = rasterize_to_bitmap(...);           // rasterize
```

The cmap subtable prefers format 12 (higher score), else format 4; if it fits in the 28KB cache it is queried from RAM, otherwise `char_to_gid_sd` reads the SD randomly per lookup (slow, but works). The cmap cache was widened from the early 2KB to **28KB** — exactly enough to hold a subset font's entire cmap subtable (e.g. Light_gb2312's fmt4, ~28KB); simhei proved that once cmap is cached, loca/glyf via SD fallback is fast enough (cmap is the per-character bottleneck). On font change `parse_font_info` unconditionally zeroes all three table-cache lengths, avoiding a "new font + stale cache" mismatch (symptom: every character renders as a question mark).

**Rasterization** uses `ab_glyph_rasterizer` (a coverage rasterizer, with `libm` for no_std soft float). It flattens quadratic curves into edges, then scan-converts per-pixel coverage. The pixel-set test is **`coverage × stroke_weight ≥ 0.35`** (below the 0.5 baseline so thin strokes don't break): the weight defaults to ×1.0 and is adjustable ×1.0–×3.0 in the Layout menu's "Stroke" item (0.1 steps, stored in `StrokeConfigStorage`). Multiplication boosts low-coverage thin horizontals dramatically (a visible fix for broken strokes) while saturated ink regions are unaffected and edges widen only slightly.

**Glyph cache**: rasterization is expensive, so cache it. The bitmap area is a **12KB flat buffer** adaptively sliced at runtime by font size: each slot holds `((px+2)+7)/8 × (px+2)` bytes (a CJK glyph is roughly px×px, 1-bit packed), slot count = 12288/slot bytes (cap 256). Big font → big slots, few of them (enough); small font → small slots, many. Circular overwrite (oldest slot reused when full). A deduplicated page is roughly a hundred glyphs, so this cache usually makes both "render current page" and "preload next page" pure blits. Glyphs whose bitmap exceeds the slot capacity (>256B) take the direct-draw fallback: not cached, rasterized and drawn pixel-by-pixel.

**The `TtfWs` workspace** is ~53KB, heap-allocated (`alloc_ws`/`free_ws`, a raw `Layout` allocation to avoid `Box::new`'s stack temporary), created on entering reading and freed on exit:

```rust
pub struct TtfWs {
    pub pts_x: [i32; 600], pub pts_y: [i32; 600], pub pts_on: [u8; 600], // glyph points
    pub end_pts: [u16; 96],                                              // contour ends
    pub cmap_cache: [u8; 28672], pub hmtx_cache: [u8; 1024], pub loca_cache: [u8; 1024],
    pub glyf_buf: [u8; 2048], pub book_buf: [u8; 2048],                  // glyf / text buffers
    pub gc_gid: [u16; 256], pub gc_w: [u8; 256], pub gc_h: [u8; 256],
    pub gc_yoff: [i16; 256],                                             // glyph cache metadata
    pub gc_bmp: [u8; 12288],                                             // 12KB flat bitmap area
    pub gc_slot_bytes: usize, pub gc_n_slots: usize, pub gc_next: usize, // slots/cursor
    pub fi: Option<FontInfo>,                                            // parsed metrics
}
```

### 2.2 Bitmap Font (src/pages/read/bitmap.rs)

Bitmap mode uses the **u8g2 wqy bitmap fonts already linked into the firmware** (menus/calendar use them all the time): no SD font file, no rasterization; rendering is a direct blit of flash bitmaps — much faster than TTF. This is the choice when paging speed matters most.

The width model is a **fixed cell**: CJK `full_w`, letters/digits `half_w` (half width, per Chinese convention), without querying real glyph widths. Pagination is then pure arithmetic (count lines/cells) and rendering blits each character at its cell origin:

```rust
// Render: draw into cells, pen_x advances by width
let w = if (ch as u32) < 0x80 { half_w } else { full_w };
if pen_x + w > max_w { wrap }
font.render_aligned(&ch_str, Point::new(pen_x, pen_y), ...);
pen_x += w;
```

Three functions mirror the TTF side one-to-one: `paginate_render_bitmap` / `compute_consumed_bitmap` / `find_prev_start_bitmap`.

**Font slimming**: the system keeps only wqy **12/14/16** (wqy15 and the wqy12b/wqy14b bold variants were removed), saving ~463KB of flash. Each full-GB2312 wqy set is about 198KB (12) / 242KB (14) / 301KB (16).

### 2.3 Font Selection

The font menu lists available fonts: it scans the SD card's **`fonts/`** directory (long filenames via LFN with 8.3 short-name fallback; `books/` holds only books), then appends the built-in "点阵16/点阵14/点阵12". The list capacity is 16 with the last 3 reserved for bitmap fonts — at most 13 TTFs are collected, so TTFs can't crowd the bitmaps out (push fails silently).

```rust
fn bm_size_of(name: &str) -> Option<u32> {
    match name { "点阵16" => Some(16), "点阵14" => Some(14), "点阵12" => Some(12), _ => None }
}
fn is_bitmap(name: &str) -> bool { bm_size_of(name).is_some() }
```

When a bitmap font is selected **no .ttf file is opened** (`font_file_ptr=null`), but a `TtfWs` **is still allocated** — its `book_buf` (2KB) is reused to read the text, so the reading infrastructure (reading `.txt`, the history stack, resume) is shared by both paths. `bm_size_of(font name)` decides at render/pagination time whether to take the bitmap or TTF path. Changing font = reopen handle + `cache_clear` (glyph cache) + `ws.fi=None` (table cache) + clear the history stack (every page boundary changed).

---

## 3. Dynamic Pagination and Navigation

### 3.1 No Whole-Book Index

A common e-reader approach is to scan the whole book on open, build a page index, and page in O(1) table lookups. This project deliberately does **not** — scanning a whole book on open (TTF must measure every character) would freeze for seconds to tens of seconds.

Instead: **byte-offset dynamic pagination**. Each page records its **start byte offset** and **end byte offset** in the `.txt` (the "bytes consumed by this page" returned by rendering). Given an offset, a page renders immediately; no global index needed.

### 3.2 Forward / Backward

- **Forward O(1)**: `ttf_offset ← ttf_end`. The offset where the current page ended is exactly where the next begins.
- **Backward O(1)**: the history stack `ttf_hist` (256 slots). Forward pushes the current page's start; backward pops it — fully symmetric and exact with forward. (When full, the oldest entry is dropped, preserving a "last 256 pages" semantic — otherwise push silently fails once full and backward would pop back to positions read early in the session.)
- **Empty stack** (first backward right after entering/jumping): nothing to pop, so `find_prev_start` computes the previous page start on the fly.

```rust
// src/pages/read/page.rs
async fn do_change_page(&mut self, next: bool) {
    if next {
        if self.ttf_at_end { /* last-page hint once, don't advance */ return; }
        if self.ttf_hist.is_full() { self.ttf_hist.remove(0); }
        let _ = self.ttf_hist.push(self.ttf_offset);
        self.ttf_offset = self.ttf_end;
    } else {
        if let Some(prev) = self.ttf_hist.pop() {
            if prev >= self.ttf_offset { return; }  // already at the top
            self.ttf_offset = prev;
        } else {
            let prev = self.find_prev_page_start();  // empty stack: compute
            if prev >= self.ttf_offset { return; }
            self.ttf_offset = prev;
        }
    }
    self.ttf_end = self.ttf_offset;
    // store rtc_fast + trigger render + persist position
}
```

Normal reading (forward then backward) almost always goes through the history stack; only "backward right after opening/jumping" takes the computed path.

### 3.3 find_prev_start: Computing the Previous Page Start (the hard part)

This is the hardest piece of the reading subsystem. The core property: the true page boundary `pk-1` **maximizes** `pk-1` subject to `pk-1 + (bytes a page consumes) ≤ cur_start` (the previous page ends at or before the current page's start).

The naive approach scans page by page from offset 0 up to `cur_start` and returns the last boundary — exact (every step is a true boundary, char-aligned) but far too slow for TTF at large sizes. This project uses **binary search + character alignment + a `≤` test**:

```rust
// src/ttf_sd.rs (TTF) / src/pages/read/bitmap.rs (bitmap), same structure
pub fn find_prev_start(..., cur_start, ...) -> u32 {
    // ① Binary search (tested only at char-aligned points): max X with X+consumed(X) ≤ cur_start
    let mut lo = 0; let mut hi = cur_start;
    while lo + 16 < hi {
        let mid = align_down_char(book_f, (lo + hi) / 2);  // char-align
        if mid <= lo { break; }
        let c = consumed_at(mid);
        if c > 0 && mid + c <= cur_start { lo = mid; } else { hi = mid; }
    }
    // ② Refine: scan down from hi for the first char-aligned P-true = pk-1 (≤32 steps)
    let mut x = align_down_char(book_f, hi);
    let mut steps = 0;
    while steps < 32 {
        let c = consumed_at(x);
        if c > 0 && x + c <= cur_start { return x; }
        if x == 0 { break; }
        x = align_down_char(book_f, x - 1);
        steps += 1;
    }
    lo  // fallback: lo is a char-aligned P-true (guaranteed by the search), never a 0-failure
}
```

Three details here were bought with real bugs:

1. **Binary search, not a full scan**: only a few dozen pages are scanned (not the whole book); acceptable at small-to-medium TTF sizes.
2. **Character alignment (`align_down_char`)**: tests run only at UTF-8 character starts. Otherwise a half-character start makes `compute_consumed` count continuation bytes as full-width `FFFD` (1 byte occupying a full cell), distorting page size and landing on a non-boundary — that page's first character renders as a "block" (half-character garbage).
3. **Test `≤`, not `==`**: `consumed(pk-1)` occasionally differs from `cur_start - pk-1` by ±1 byte (TTF advance float rounding, buffer edges). With `==` a miss returns 0 → **jump back to page 1**; `≤` always has a solution (`lo` as fallback) and never skips pages.

### 3.4 Jump and Bookmarks

- **Jump = percentage**: the menu takes 0–100% (short press ±1, long press auto-repeats with acceleration: steps 1→5→10→50→100→200→400, with a preview bar). Confirming computes `off = input_pct × file_len / 100`; `jump_to_offset` clears the history stack (stale after a big jump), sets the offset, writes `TTF_RESUME_OFFSET`, and triggers a render.
- **Bookmarks = byte offsets**: the `.log` file stores a `Vec<u32>`; `[0]` is the last position (**low 31 bits = offset, bit31 = reached-the-end flag**), `[1..]` are bookmarks. The bookmark UI is two-level, "list + action popup":
  - **List**: row 0 = add bookmark (confirming adds the current position, deduplicated, cap 100), rows 1..n = existing bookmarks (each shown as `X.XXX%`), row n+1 = cancel; below sits a 1/3-height preview box following the selection.
  - **Action popup**: confirming a bookmark opens a two-row popup, "Open (default) / Delete"; only a second confirm executes — add, open, and delete are merged into one entry point.
  - **Preview**: reads 512B from the `.txt` at the bookmark offset, trimmed to UTF-8 character boundaries before rendering (512%3=2, so Chinese is almost always cut mid-character; `from_utf8` is all-or-nothing — without trimming, the whole preview fails and shows blank). Drawing does **not** use a `display.clipped()` view — under rotation the `DrawTarget` bounding_box is the physical size (height 300), and clipping silently truncates at y=300, so the preview showed only its first two lines (a historical bug). Instead lines are wrapped manually and drawn line-by-line with `render_aligned` straight to the display, stopping via an out-of-range `break` at the preview box's bottom edge.
- **Last page**: if `offset + consumed ≥ file_len` at render time, `ttf_at_end` is set; the true last page displays normally, and pressing forward again overlays a one-time "already the last page" hint. The bit31 flag is written into `.log[0]` on page turns, and the book list shows 100.000% directly from it.

### 3.5 Book List: Subdirectories and the Progress Column

The book list is no longer flat: **subdirectories under `books/` can be entered**, listed before the books (inside a subdirectory the first row is `..` to go up; only two levels — no nesting inside subdirectories).

- **Entering** switches directory in place via `change_dir` (LFN names resolved to on-disk SFN aliases through `find_entry_by_name`); **going up** reopens `books/` from the SD root and replaces+closes the old handle (freeing the VolumeManager's directory slot). After a level change the subdirectory/book lists and progress are rescanned and the cursor resets to 0.
- **Level and cursor survive deep sleep**: the resumed book's subdirectory is stored in `RESUME_SUBDIR`; entering the list normally restores the directory and selection from `LIST_SUBDIR` + `LIST_BOOK_IDX` (the last-opened book's subdirectory and index) — all rtc_fast, zeroed on power loss.
- **Each book shows its reading progress** `X.XXX%` on the right (offset ×100_000 / file length, u64 intermediates to avoid overflow; bit31 set → `100.000%`). Long titles are truncated with `…` measured at wqy16's real rendered width so they don't overlap the progress column.

### 3.6 Arrow Keys and "Swap Arrow Keys"

The direction keys (Key1/Key2), short and long, funnel into `dir_key_short`/`dir_key_long` (the `down` parameter means "down/+1/next page"). The "Swap arrow keys" menu item toggles `swap_keys`; `bind_event` swaps the two physical keys' `down` values accordingly — when enabled, directions are swapped **everywhere inside reading** (paging/menus/jump/layout/fonts/bookmarks/book list), and the flag is persisted to flash (`ReadingFlagsStorage`, magic `PGSW`), surviving reboots.

---

## 4. Resume Reading: Back to the Last Position Across Reboots

Resume uses several complementary stores for different scenarios:

| Store | Location | Survives | Purpose |
|---|---|---|---|
| `TTF_RESUME_OFFSET` | rtc_fast RAM | `reboot_sleep` deep sleep | Instant resume on wake |
| `RESUME_SUBDIR` / `LIST_SUBDIR` / `LIST_BOOK_IDX` | rtc_fast RAM | `reboot_sleep` deep sleep | Resume/list directory and cursor |
| `.log[0]` | SD card file | any reboot | Authoritative persistence + list progress |
| `SleepPageStorage` (sentinel 100) | NVS flash | any reboot | Restore "was reading" when an RTCIO wake loses rtc_fast |

**Entry flow**:

1. `ReadPage::new` reads `PAGE_INDEX` (the selected book) and `TTF_RESUME_OFFSET`, setting `ttf_resume=true`; it also restores `RESUME_SUBDIR` (resume) or `LIST_SUBDIR/LIST_BOOK_IDX` (list level).
2. `get_page_vec` opens the `.txt` to get the file length; with `ttf_resume=true` the preset offset is kept (clamped to file_len).
3. `get_log_vec` reads `.log`; with `ttf_resume=true` the `.log[0]` overwrite is skipped (rtc_fast is authoritative), otherwise the low 31 bits of `.log[0]` restore the offset. **The restored offset is then synced back into `TTF_RESUME_OFFSET`** — this fixed a pitfall: opening a book zeroed `TTF_RESUME_OFFSET` and `.log[0]` restored the on-screen offset without syncing it back to rtc_fast, so "sleep without turning a page, wake → back to page 1".

**Note**: rtc_fast does **not survive `software_reset()`** (a CoreSw reset clears it), so mode switches always use `reboot_sleep()` (deep sleep), never `software_reset`. An RTCIO wake (button wake) also loses rtc_fast — hence the NVS sentinel fallback.

### Persisting Layout and Key Preferences (storage.rs)

Font size, line gap, rotation, font name, stroke weight, and the arrow-key swap all land in flash and are restored when a book opens:

```rust
// src/storage.rs
pub struct ReadingSettings {
    pub ttf_px: u8,        // font size 12–40
    pub line_gap: i8,      // line gap -10..+10 (negative tightens)
    pub rotate: u8,        // rotation 0–3
    pub font_name: [u8; 32], // null-terminated font name (.ttf or 点阵XX)
}
// Two independent small structs appended to the storage chain:
pub struct ReadingFlagsStorage { pub magic: u32, pub swap_keys: u8 } // magic "PGSW"
pub struct StrokeConfigStorage { pub magic: u32, pub weight_x10: u8 } // magic "STKE", 10..=30
```

`font_name` validation is lenient: accepts `.ttf` or a built-in bitmap name (`is_ttf || is_bitmap`), falling back to `font.ttf`, so garbage bytes from uninitialized flash aren't mistaken for a font name and break opening; reads are clamped to valid ranges. New structures are always **appended to the end of the storage chain** — touching existing fields would shift later offsets and lose user data.

---

## 5. Screen Fit and Rotation

Three layout modules are chosen at compile time by screen (feature gate):

| Module | Screen | Native resolution | Default rotation |
|---|---|---|---|
| `layout_400x300` | 4.2" | 400×300 | Rotate90 |
| `layout_176x264` | 2.7" | 176×264 | Rotate0 |
| `layout_128x296` | 2.9" | 296×128 | Rotate90 |

Each module provides rotation-aware geometry:

```rust
pub fn visual_width() -> u32 { if swapped() { DISPLAY_HEIGHT } else { DISPLAY_WIDTH } }
pub fn visual_height() -> u32 { if swapped() { DISPLAY_WIDTH } else { DISPLAY_HEIGHT } }
pub fn text_width() -> u32 { visual_width() }       // text area full width (flush left)
pub fn text_left_margin() -> i32 { 0 }
pub const PROGRESS_AREA_HEIGHT: u32 = 8;             // bottom progress bar reserve (bar is 3px)
```

**4-way rotation**: `ROTATE_STATE` is an `AtomicU8`; the menu's "Rotate screen" adds +90° each press (the menu stays open for consecutive rotations). `PROGRESS_AREA_HEIGHT` dropped from the early 20 to 8 (the bar is really only 3px tall; the old value wasted nearly a full text line).

**Menu adaptivity**: the reading menu (9 items) computes row height from `visual_height` (even split, clamped to 12–24px). Small screens (vh<200, e.g. the 2.7" landscape at 176 tall) can't fit a fixed 9×24 menu, so they use smaller rows + the wqy12 font; large screens keep wqy14. The bookmark list has a compact mode too (preview takes half the height, rows 12/14).

**Line counting**: `max_lines = (text area height − one line of ink) / (line height + gap) + 1` — the budget first reserves one full line of ink for the last line (a negative gap compresses spacing, not ink), then splits the remainder into "row slots"; `line_h = text area height / max_lines` distributes the slack so text fills the screen. The text area height is `visual_height − PROGRESS_AREA_HEIGHT − 12` (status row battery icon 9px + progress bar band). **Negative line gaps** shift everything up: `shift = min(line_gap, 0)`, first line starts at y = 2+shift and the budget gains the same amount (`text_area_h − shift`), leaving the last-line ink constraint unchanged — inter-line space compresses and the first line's top spacing tightens in step. Body rendering, backward pagination, and preloading must share this exact formula so line counts agree.

---

## 6. Key Data Structures

```rust
// src/pages/read/page.rs
pub struct ReadPage {
    running: bool, reading: bool, need_render: bool,
    need_text_render: bool,                           // size/gap/rotation changed in menu → re-render text
    choose_index: u32,                                // list cursor (list level=row, reading=book index)
    menus: Option<Vec<String<BOOK_NAME_MAX>, 40>>,    // books in the current directory
    cur_subdir: Option<String<BOOK_NAME_MAX>>,        // current books subdir (None=root)
    subdirs: Vec<String<BOOK_NAME_MAX>, 20>,          // root-level subdirectories
    log_vec: Option<Vec<u32, LOG_VEC_MAX>>,           // .log ([0]=pos|bit31, [1..]=bookmarks)
    menu_state: MenuState,
    rotate: u8,                                       // rotation 0..3
    swap_keys: bool,                                  // arrow-key swap (flash-persisted)
    books_dir_ptr: *mut ActualDirectory<'static>,     // raw ptr valid inside run()
    font_file_ptr: *mut ActualFile<'static>,          // TTF handle (null for bitmap)
    ttf_offset: u32, ttf_end: u32, ttf_file_len: u32, // pagination state
    ttf_hist: Vec<u32, 256>,                          // backward history stack
    ttf_at_end: bool, ttf_last_hint: bool,            // last-page flag / one-time hint
    ttf_px: f32, line_gap: i32, stroke_w: u8,         // layout (size/gap/stroke ×10)
    ttf_ws: Option<&'static mut TtfWs>,               // render workspace
    font_name: String<32>, font_list: Vec<String<32>, 16>,
    bookmark_preview: String<ONE_PAGE_CONTENT_LEN>,   // bookmark preview (512B, boundary-trimmed)
    // ...
}

enum MenuState {
    Closed, Popup { menu_index: u32 },
    JumpInput { input_pct: u8 },
    Layout { cursor: u8, editing: bool },             // 0=size 1=gap 2=stroke 3=reset 4=back
    FontList { font_index: u32 },
    BookmarkList { bm_index: u32 },                   // 0=add 1..=n=bookmarks n+1=cancel
    BookmarkAction { bm_index: u32, action: u32 },    // action 0=open 1=delete
}
```

Menu items (9): `返回书单 (book list) / 书签 (bookmarks) / 跳转进度 (jump) / 排版 (layout) / 字体 (font) / 旋转屏幕 (rotate) / 方向键互换 (swap keys) / 睡眠 (sleep) / 取消 (cancel)`. The "Swap arrow keys" row dynamically appends its on/off state. The layout menu has 5 rows: **size** (12..40, ±1 steps) / **gap** (-10..+10) / **stroke** (×1.0–×3.0) / **reset** (size 20, gap 1, stroke ×1.0) / **back**; changing size or stroke clears the glyph cache, and any change of size/gap/stroke clears the history stack (every page boundary changed).

---

## 7. Key Files

| File | Responsibility |
|---|---|
| `src/ttf_sd.rs` | TTF parsing + ab_glyph rasterization + glyph cache + pagination/rendering |
| `src/pages/read/bitmap.rs` | Bitmap-font pagination/rendering (u8g2 wqy, fixed width) |
| `src/pages/read/page.rs` | `ReadPage`: reading logic, navigation, menus, resume, font selection |
| `src/pages/read/layout_*.rs` | Three screen adapters (geometry/rotation/default orientation) |
| `src/pages/read/mod.rs` | Module declarations + `reading_task` |
| `src/pages/main_page.rs` | `EReadPage` entry (triggers the reading-mode reboot) |
| `src/storage.rs` | `ReadingSettings` / `StrokeConfigStorage` / `ReadingFlagsStorage` persistence |
| `src/sleep.rs` | `reboot_sleep` (the deep sleep used for mode switches) |

---

## 8. Performance and Trade-offs

- **TTF vs bitmap**: TTF offers arbitrary sizes and rich glyphs but rasterizes slowly; bitmap is extremely fast but limited (GB2312) with fixed sizes. Users get both; pick bitmap for speed.
- **Preloading yields CPU by time**: after rendering the current page, `preload_glyphs` pre-rasterizes the next page's glyphs (cache-only, no drawing). Soft-float rasterization is heavy (per-glyph cost grows with the square of the size: 16px ~4ms, 40px ~40ms; a page is hundreds of glyphs), so preloading yields via `Timer::after(1µs)` every **≥10ms** — driven by time, not glyph count, otherwise the yield interval at large sizes stretches to hundreds of ms and keys feel dead after a page turn. With the yields, the display task flushes the panel in parallel: the screen updates first, preloading finishes in the background.
- **Backward `find_prev_start`**: binary search + char alignment + `≤`, scanning only a few dozen pages. Bitmap is pure arithmetic and fast; small-to-medium TTF is acceptable; large TTF is slower — for those, read in bitmap mode.
- **No whole-book index**: entering a book has zero delay; the price is computing on an empty backward stack (but almost all backward moves hit the history stack in O(1)).
- **Font slimming**: only wqy 12/14/16 remain, saving ~463KB of flash.

The reading subsystem is the largest piece of engineering on this device, and the one with the most scars — from static pagination, to a whole-book page index, to byte-offset dynamic pagination + history stack + `find_prev_start`, the paging model went through several iterations. The final design balances "zero-delay book open, instant forward, near-instant backward, accurate content".
