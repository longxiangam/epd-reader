# Part 9: Error Handling and Debugging

> 中文版: [../09-错误处理与调试.md](../09-错误处理与调试.md)

> Building an ESP32-C3 E-Paper Reader in Rust · Article Series

## Introduction

Debugging in embedded development is painful. Without GDB your only tool is the `printf` sledgehammer; when the device suddenly reboots you don't know why; when the program crashes you don't even get a call stack.

This project's error-handling system — a custom panic handler, a Flash error log, and call-stack backtraces, three layers in total — makes the "black box" device produce genuinely useful debugging information.

## 1. The Custom Panic Handler

Rust has no default panic handling in a `#![no_std]` environment. You must implement `#[panic_handler]` yourself:

```rust
// src/panic.rs:9-100 (excerpt)
#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    println!("!! 发生panic错误 !!");

    // 1. Build the error message
    let mut error_msg = heapless::String::<200>::new();

    // File location: file:line:column
    if let Some(location) = info.location() {
        let _ = error_msg.push_str("location: ");
        let _ = error_msg.push_str(location.file());
        let _ = write!(error_msg, ":{}:{}", location.line(), location.column());
        let _ = error_msg.push_str("\n");
    } else {
        let _ = error_msg.push_str("location: unknow\n");
    }

    // Panic message
    let _ = error_msg.push_str("error: ");
    let _ = write!(error_msg, "{}", info.message());
    let _ = error_msg.push_str("\n");

    // 2. Capture the call stack
    let _ = error_msg.push_str("stack:\n");
    let backtrace = Backtrace::capture();
    let mut stack_count = 0;
    for frame in backtrace.frames() {
        if stack_count < 10 {   // cap the stack depth at 10 frames
            let _ = write!(error_msg, "  #{} 0x{:x}\n", stack_count, frame.program_counter());
            stack_count += 1;
        } else { break; }
    }
    if stack_count == 0 {
        let _ = error_msg.push_str("call stack information is not available\n");
    }
    if error_msg.len() > 200 { error_msg.truncate(200); }  // matches the storage field width

    // 3. Save to Flash (fall back to the default value if the read fails,
    //    so a panic can always be recorded)
    let mut error_storage = match ErrorLogStorage::read() {
        Ok(storage) => storage,
        Err(_) => ErrorLogStorage::default(),
    };
    error_storage.error_count += 1;
    error_storage.last_error = error_msg;   // store the new error; the old one is dropped
    let _ = error_storage.write();

    // 4. Print the full error to the serial console
    println!("错误计数: {}", error_storage.error_count);
    println!("最后错误: {}", error_storage.last_error);

    // 5. Wait for the Flash write to finish
    for _ in 0..1000000 {
        core::hint::spin_loop();
    }

    // 6. Halt (software_reset is commented out — a human is expected to intervene)
    loop { }
}
```

### Key Design Decisions

**Why no automatic reboot?** The code ends in `loop {}` rather than `software_reset()` (the reset line is commented out, even though the log still prints "the system will reboot"). This is deliberate — an automatic reboot after a panic risks a crash loop (panic → reboot → panic), and the reboot would overwrite `last_error`, hiding the first crime scene forever. Letting the device halt in the panic state means the full error is visible over serial, and after a manual reset it can be reviewed in the DebugPage.

**Why wait with `spin_loop()`?** A panic handler cannot use async (`.await`) or blocking delays (they need hardware timers). `spin_loop()` is a busy-wait hint to the CPU — effectively a no-op that gives the Flash write time to complete.

**Defensive writing**: if `ErrorLogStorage::read()` fails (e.g. a panic before Flash was initialized), `ErrorLogStorage::default()` is used instead, so the error is always recorded; the message is truncated to 200 characters before writing, strictly matching the `last_error: String<200>` field so UTF-8 is never silently cut in half.

## 2. Call-Stack Backtraces

### 2.1 Backtrace Capture

The `esp-backtrace` crate (0.19) provides stack unwinding for the RISC-V architecture:

```rust
// src/panic.rs:40-49
let backtrace = Backtrace::capture();
for frame in backtrace.frames() {
    let _ = write!(error_msg, "  #{} 0x{:x}\n", stack_count, frame.program_counter());
}
```

esp-hal v1.x uses `Backtrace::capture()`, which returns a structured backtrace iterated via `.frames()` to get each frame's program counter (PC) address. The old version returned a plain `[Option<u32>; 10]` array. If not a single frame was captured (a broken frame-pointer chain), the message "call stack information is not available" is written explicitly, so a broken chain is not mistaken for "no crash".

### 2.2 Frame Pointer Support

The backtrace depends on frame pointers, enabled in the build options:

```toml
# .cargo/config.toml
rustflags = [
  "-C", "link-arg=-Tlinkall.x",
  "-C", "force-frame-pointers",
]
```

Frame pointers are the "linked list" of the call stack — each stack frame begins with the address of the previous one. Walking that chain yields the full call stack. Rust omits frame pointers when optimizing to save a register; `force-frame-pointers` forces them to be kept.

### 2.3 Resolving Addresses

The captured PC addresses such as `0x42001234` are bare addresses. To resolve them to function names and source locations you need the ELF file produced by the build:

```bash
# Resolve an address with addr2line
riscv32-esp-elf-addr2line -e target/riscv32imc-unknown-none-elf/release/epd-reader 0x42001234
```

This prints something like `src/wifi.rs:123`. The device cannot do this resolution itself, but recording the addresses in the serial log makes it possible to pinpoint the crash location in post-mortem analysis.

## 3. Error Log Persistence

### 3.1 The Storage Structure

```rust
// src/storage.rs:168-172
#[derive(Debug, Default, Clone)]
pub struct ErrorLogStorage {
    pub error_count: u32,
    pub last_error: heapless::String<200>,
}
```

The error log lives in the NVS partition of Flash (block 7 of the chain described in Part 6) and contains:
- `error_count`: the cumulative number of panics
- `last_error`: the details of the most recent panic (location + message + up to 10 stack frames, 200 characters total)

### 3.2 The Boot-Time Check

If the `enable_debug` feature is enabled, the boot sequence checks for an error log:

```rust
// src/pages/main_page.rs:93-103
#[cfg(feature = "enable_debug")]
if let Ok(error_log) = crate::storage::ErrorLogStorage::read() {
    if error_log.error_count > 0 {
        println!("=== 检测到错误日志，进入调试模式 ===");
        println!("错误计数: {}", error_log.error_count);
        println!("最新错误信息:");
        println!("{}", error_log.last_error);
        println!("================================");
        page_index = 5;  // go straight to the DebugPage
    }
}
```

This lets the developer review the details of the last crash in the DebugPage after a reboot. The check sits in `MainPage::init`, right after the default-home-page selection; note it runs before the NVS "restore last page" step — if Flash holds a valid page record, the restored value overrides `page_index = 5`, so the debug page only actually opens when no valid page record exists (cold boot / first crash), while the serial log always prints the error content.

## 4. The DebugPage

The DebugPage provides a simple screen for viewing the error log, with a clear action:

- **Key1 long press (on release)**: clears the error log (`error_count = 0`, `last_error` emptied) and redraws
- **Key3 short press**: exits the DebugPage

Its `render()` re-reads `ErrorLogStorage` on every redraw and draws `last_error` line-wrapped onto the screen with `embedded-text`'s `TextBox` (the wqy12 bitmap font); when there is no error it displays "没有错误日志" (no error log).

## 5. Serial Debugging

### 5.1 esp-println

```rust
use esp_println::println;
println!("wifi connected: {:?}", state);
```

`esp-println` writes debug output over UART. When running `cargo run`, `espflash monitor` opens the serial monitor automatically and all `println!` output appears in the terminal.

### 5.2 The log Framework

The project also enables the `log-04` feature, so the `log` crate's leveled logging is available:

```rust
use log::trace;
trace!("test trace");  // only compiled in when the project uses log
```

The `log` framework supports the five levels `error!`, `warn!`, `info!`, `debug!`, and `trace!`; unwanted levels can be filtered at compile time to reduce serial traffic.

### 5.3 Runtime Diagnostic Prints

Beyond the error paths, the code carries several periodic "health check" prints, grouped by `[tag]` prefix:

**Battery `[bat]`** — the battery sampling task reads the ADC every 60 seconds and prints one line of conversion diagnostics on success:

```rust
// src/battery.rs:69-73
// pin = pin-voltage estimate under the 11dB-attenuation theoretical full scale;
// bat = battery voltage estimated as raw×2 (the 2× gap is the divider ratio),
// both for comparison against a multimeter reading of the divider midpoint
println!("[bat] raw={} pin≈{}mV bat≈{}mV 电量{}%", adc_value, pin_mv, v.voltage, v.percent);
```

Occasional ADC read failures are retried: every 2ms, up to 50 times (`tries >= 50` gives up on the round, keeps the previous percentage, and prevents a lock-holding infinite loop); below 20% an extra low-battery warning is printed. The resulting percentage is also written to `LAST_BATTERY_PERCENT` in rtc_fast, so the render path can read it synchronously via `battery::last_percent()` with no await. The divider pin is board-dependent: GPIO2 on the new v2 board, GPIO4 on the old one (`crate::board::BatAdcGpio`).

**Board boot diagnostics `[diag]`** — each call to `board::print_boot_diag()` increments `RTC_FAST_BOOT_SEQ` in rtc_fast and prints it:

```rust
// src/board/mod.rs:82-94 (excerpt)
pub fn print_boot_diag() {
    let (when_ts, clk_sync) = crate::worldtime::diag_rtc_fast();
    // boot_seq incrementing across deep sleep = rtc_fast retention OK; back to 1 = lost
    println!("[diag] rtc_fast boot_seq={} when_sleep_ts={} clk_sync={}", ...);
}
```

Together with `worldtime::diag_rtc_fast()` (which dumps the pre-sleep timestamp and the sync flag), one line tells you whether rtc_fast survived this wake-up — the primary evidence when debugging wake-up classification (timed vs button wake). The serial log also carries `[wake]` (wake reason + weather staleness), `[time]` (time restored from / written to the PCF8563), `[sd]` (SD volume open result), `[page]` (the page entered this boot) and more, all in the same style.

## 6. Common Failure Patterns

### 6.1 unwrap() Panics

Embedded code leans heavily on `unwrap()`:

```rust
let mut epd = EpdControl::new(...).unwrap();
spawner.spawn(display::render(...)).unwrap();
```

If `EpdControl::new` fails (e.g. an SPI communication error) the program panics. The panic handler records it into Flash, visible in the DebugPage after the next start.

### 6.2 Stack Overflow

All of the ESP32-C3's main DRAM now goes to the call stack (the heap moved to a separate dram2 segment — see 6.3), and deep async call chains can still put large locals on the stack. The project's counters are twofold: large structures are processed in chunks — the `HolidayStorage` chunked write (Part 6) only ever puts 128 bytes on the stack — and a watchdog watchpoint at `_stack_end` catches overflow immediately instead of silently corrupting the heap.

### 6.3 Out of Memory

The heap sits in the ~64KB dram2 segment released by the bootloader (`esp_alloc::HEAP.add_region`), leaving the whole main DRAM to the stack — the two no longer squeeze each other. But 64KB still demands discipline: the ~33KB of large HTTP/TLS buffers live in `.bss` under `WIFI_LOCK` serialization rather than on the heap, and heap-hungry code (JSON parsing, page menus) must watch its peaks. When memory runs out `esp-alloc` returns an allocation failure, and the surrounding `unwrap()`/`expect()` panics.

## 7. The Debug Toolchain

| Tool | Purpose |
|------|---------|
| `espflash monitor` | Serial monitor for `println!` output |
| `addr2line` | Resolves PC addresses to source locations |
| `esp-backtrace` | RISC-V call-stack backtraces |
| `enable_debug` feature | Boots straight into the debug page |
| Flash error log | Persists panic info for post-restart review |
| `[bat]`/`[diag]` diagnostic prints | Battery conversion, rtc_fast retention state, wake reason |

## Summary

| Topic | Key Point |
|-------|-----------|
| Panic Handler | Custom handler; location + message + 10 stack frames written to Flash |
| Backtrace | `force-frame-pointers` + `esp-backtrace` 0.19 |
| Error log | Persisted in Flash; viewable and clearable in the DebugPage |
| Serial debugging | `esp-println` + the `log` framework + `[bat]`/`[diag]` diagnostics |
| Memory layout | Heap in the dram2 segment, all of main DRAM for the stack, HTTP buffers in `.bss` |
| Address resolution | Post-mortem PC-to-source mapping with `addr2line` |

The core challenge of embedded debugging is **lack of information** — you cannot attach a debugger or watch variables in real time. This project's strategy is to **capture and persist as much crash information as possible**, making post-mortem analysis possible.

In the next article we dive into the e-book reading subsystem — TTF vector / bitmap dual font modes, byte-offset dynamic pagination, resume, and bookmarks.

---

> Previous: [Part 8: UI Components and the Page System](../08-UI组件化与页面系统.md) · Next: [Part 10: E-Book Reading — Vector Fonts / Bitmap Fonts / Dynamic Pagination](../10-电子书阅读与矢量字体.md)
