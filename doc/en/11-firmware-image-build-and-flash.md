# Part 11: Firmware Image Build and Flash — From ELF to .bin

> 中文版: [../11-固件镜像构建与烧录.md](../11-固件镜像构建与烧录.md)

## Introduction

For day-to-day development, `cargo run --release --features epd4in2,board_v2` is a single command that compiles + flashes + opens a serial monitor — espflash receives the ELF file and does the format conversion itself. But there are always cases where an ELF isn't enough: sending someone a firmware file, flashing with esptool or a web flasher, producing a factory image. That's when the build artifact needs to become a `.bin` firmware image.

This article covers three things: the relationship between ELF and bin, the format of ESP firmware images, and how to generate and flash this project's two bin kinds (whole-flash image / app-only).

## 1. Build Artifacts: What's in an ELF

```bash
cargo build --release --features epd4in2,board_v2
```

The artifact is `target/riscv32imc-unknown-none-elf/release/epd-reader` — an ELF file for the RISC-V architecture. It contains:

- **Loadable sections**: `.text` (code), `.rodata` (constants/font tables), `.data`/`.bss` (data) — the content that actually goes into flash;
- **Debug info**: symbol table, DWARF line info — for the debugger/addr2line only, never flashed.

When flashing an ELF, espflash parses the section table itself, drops the debug info, and wraps the loadable sections in the ESP image format. A `.bin` is exactly that "header-wrapped loadable content" written to a file, recognizable by any flashing tool without espflash.

**features = screen + board**: the firmware is defined by two feature groups, and images are not interchangeable across combinations —

| Feature combination | Hardware |
|---|---|
| `epd4in2,board_v2` | 4.2" panel + v22 new board |
| `epd4in2,board_v1` | 4.2" panel + old board |
| `epd2in9,board_v1` | 2.9" panel + old board |
| `epd2in7,board_v1` | 2.7" panel + old board |

Commands below use `epd4in2,board_v2` as the example; substitute the feature set for other combinations.

## 2. ESP Firmware Image Format

### 2.1 Image Header and Section Table

ESP32-family firmware images (bootloader and application alike) share one format; the first byte is always the magic `0xE9`:

```
E9 | segment count | SPI mode | SPI size/speed | entry addr 4B | extended header | segment table N × (addr, len) | segment data... | SHA256
```

On power-up the bootloader copies each segment: `DROM`/`FLASH` segments stay in flash behind the MMU, `IRAM`/`DRAM` segments are copied to RAM, and finally it jumps to the entry address. A 32-byte SHA256 at the end serves verification.

The application image's extended header carries an `esp_app_desc` — in this project, `esp_bootloader_esp_idf::esp_app_desc!()` in `main.rs` fills in the project name and version; the version info read by `esptool image_info` and espflash comes from there.

### 2.2 Second-Stage Bootloader and Partition Table

The ESP32-C3 flash layout has two stages:

1. **First-stage bootloader** (burned into ROM): the power-on reset vector; it only loads the second-stage bootloader from flash 0x0 into RAM and runs it;
2. **Second-stage bootloader** (bootloader, flashed at 0x0): parses the **partition table** at 0x8000, finds a type=app partition, validates the image there, and boots it.

In this project's `partitions.csv` the Offset column is empty, so espflash/IDF allocate by convention. The final layout (printable via `espflash partition-table ./partitions.csv`):

```
0x00000  second-stage bootloader   (~28KB)
0x08000  partition table           (4KB)
0x09000  nvs      data/nvs         (24KB)   ← WiFi credentials + all *Storage settings
0x0F000  phy_init data/phy         (4KB)
0x10000  factory app  type=app     (3MB)    ← the app image goes here
0x310000 storage  data/fat         (128KB)  ← after factory; book progress and other non-SD data
```

**Key point**: the `nvs` and `storage` data partitions sit between/after the bootloader and app regions. Flashing a whole-flash image overwrites those regions too — that's why "whole-flash image = data wiped".

## 3. Generating the Images

### 3.1 Whole-flash merged image (bootloader + partition table + app)

```bash
cargo build --release --features epd4in2,board_v2
espflash save-image --chip esp32c3 --partition-table ./partitions.csv \
  --merge --flash-size 4mb \
  target/riscv32imc-unknown-none-elf/release/epd-reader \
  dist/epd-reader-merged.bin
```

`--merge` stitches the second-stage bootloader (espflash's built-in ESP-IDF bootloader), the partition table, and the app image together at their respective offsets into one file the size of the whole flash (4MB, including 0xFF padding). The `App/part. size` line in the output gives the app's real size — currently 2,157,536 bytes (≈2.1MB) in this project, 68.6% of the factory partition.

### 3.2 App-only image (keeps data)

In the merged image the app starts at a fixed `0x10000`; cut that range out with `tail`/`head`:

```bash
tail -c +$((0x10000+1)) dist/epd-reader-merged.bin | head -c 2157536 > dist/epd-reader-app.bin
```

- `tail -c +N` starts at the Nth byte (1-based), hence `0x10000+1`;
- `head -c` trims to the app's real size, leaving out the trailing 0xFF padding and the storage partition;
- **the size number changes with the code; always take it from the `App/part. size` line of save-image**.

The trimmed file starts with `0xE9` — a standard app image. Flashed at the factory partition offset, it keeps **nvs (WiFi/all settings) and storage (book data) intact**.

## 4. Flashing Commands

```bash
# Daily updates (keeps data, recommended)
espflash write-bin 0x10000 dist/epd-reader-app.bin --chip esp32c3

# Whole-device image (wipes all data; for board swaps/factory distribution only)
espflash write-bin 0x0 dist/epd-reader-merged.bin --chip esp32c3

# Equivalent esptool invocation
esptool --chip esp32c3 write_flash 0x10000 dist/epd-reader-app.bin

# Daily development still uses the runner (direct ELF flash + serial monitor;
# the runner in .cargo/config.toml already carries --partition-table ./partitions.csv)
cargo run --release --features epd4in2,board_v2
```

## 5. Notes

- `dist/` artifacts are not committed (build products); `epd-reader-merged.bin` is always 4MB — not because "the firmware is 4MB" but because it is "the size of the whole flash";
- No manual erase is needed before flashing `epd-reader-app.bin`; esptool/espflash erase the affected sectors automatically on write;
- Both bins depend on the build's feature combination: changing the screen (2in7/2in9/4in2) or the board (board_v1/v2) requires a rebuild; images are not interchangeable;
- `--flash-size` must match the real flash capacity (4MB on this board) so the merged image pads to the correct boundaries.
