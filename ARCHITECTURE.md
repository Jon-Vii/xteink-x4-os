# xteink-x4-os — Architecture

Bare-metal Rust firmware for the Xteink X4 e-ink reader.
Target: `riscv32imc-unknown-none-elf` (ESP32-C3, single-core RISC-V RV32IMC, no FPU, no A-extension atomics).

> ESP32-C3 does **not** implement the A (atomic) extension. Atomic operations are provided by the `portable-atomic` crate, gated on the `unsafe-assume-single-core` feature — appropriate here because the C3 is genuinely single-core, so disabling interrupts is a sound CAS implementation. `esp-hal` enables this feature for us; do not enable `portable-atomic/critical-section` (that is the path the Xtensa S2/S3 chips take). Do not assume `core::sync::atomic` operations are lock-free on this target.

---

## Table of Contents

1. [Guiding Principles](#1-guiding-principles)
2. [Memory Budget](#2-memory-budget)
3. [Crate Workspace Layout](#3-crate-workspace-layout)
4. [Async Runtime](#4-async-runtime)
5. [Display Subsystem](#5-display-subsystem)
6. [Peripheral Model](#6-peripheral-model)
7. [Storage & Flash Layout](#7-storage--flash-layout)
8. [Power Management](#8-power-management)
9. [Data-Oriented Design](#9-data-oriented-design)
   - [9.1 DOD Invariants](#91-dod-invariants)
10. [SPI Command Encoding](#10-spi-command-encoding)
11. [Build System](#11-build-system)

---

## 1. Guiding Principles

| Principle | Rationale |
|-----------|-----------|
| **No heap** | 380 KB DRAM, no PSRAM; fragmentation is fatal |
| **No `std`** | `#![no_std]` everywhere; `alloc` crate also forbidden except in explicitly marked crates |
| **Data-oriented layout** | Flat arrays over pointer-chasing graphs. SRAM access is single-cycle (no D-cache), so the win isn't "cache hits" — it's deterministic latency, predictable working sets, and avoiding heap fragmentation in 380 KB |
| **Cooperative concurrency** | Single core — Embassy tasks yield at `.await` points; no preemption, no locks except `CriticalSection` for ISR shared state |
| **Streaming over buffering** | The 48 KB framebuffer is the single source of truth; panels are driven in horizontal band slices over SPI DMA, never double-buffered in RAM |
| **Explicit lifetimes, not refcounting** | Peripheral handles are `'static` singletons; borrowing rules enforced at compile time, not runtime |

---

## 2. Memory Budget

### ESP32-C3 SRAM regions

| Region | Size | Use |
|--------|------|-----|
| IRAM (instruction) | ~32 KB reserved by chip | Exception vectors, ISR stubs, hot-path functions tagged `#[link_section = ".iram0.text"]` |
| DRAM (data) | **~380 KB usable** | Everything below |

### DRAM allocation plan

```
 ┌─────────────────────────────────┐  0x3FCA_0000
 │  .bss / .data (static globals)  │  ~4 KB
 ├─────────────────────────────────┤
 │  Embassy executor stack         │  8 KB  (main task)
 │  Task stacks (×N)               │  4 KB each, statically declared
 ├─────────────────────────────────┤
 │  Framebuffer (800×480 / 8)      │  48 KB  — single buffer, 1 bpp
 ├─────────────────────────────────┤
 │  SPI DMA band buffer            │  4 KB   — one horizontal band (400 px × 80 lines / 8)
 ├─────────────────────────────────┤
 │  Wi-Fi / esp-wifi heap          │  ~72 KB (esp-wifi static pools)
 ├─────────────────────────────────┤
 │  Remaining free headroom        │  ~240 KB
 └─────────────────────────────────┘
```

**Hard rules**:
- `STATIC_FRAMEBUFFER: [u8; 48_000]` declared once in `display::fb`.
- DMA band buffer `DMA_BAND: [u8; 4_000]` lives in a `#[link_section = ".dma_buffer"]` to guarantee DMA-accessible placement.
- No `Vec`, no `Box`, no runtime allocation anywhere in firmware crates.

---

## 3. Crate Workspace Layout

```
xteink-x4-os/
├── Cargo.toml                  # workspace
├── ARCHITECTURE.md
│
├── fw/                         # firmware binary crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # Embassy entry point, task spawning
│       └── tasks/
│           ├── display.rs      # display driver task (SPI2)
│           ├── input.rs        # ADC polling for nav buttons + GPIO3 for power
│           ├── power.rs        # sleep/wake state machine
│           └── wifi.rs         # esp-wifi task
│
├── hal-ext/                    # thin ESP32-C3-specific wrappers (no_std lib)
│   └── src/
│       ├── spi_dma.rs          # async SPI DMA helpers
│       ├── rtc.rs              # RTC / deep sleep
│       └── nvm.rs              # NVS flash key-value
│
├── display/                    # display driver lib (no_std, no alloc)
│   └── src/
│       ├── lib.rs
│       ├── fb.rs               # framebuffer type + blit ops
│       ├── epd.rs              # EPD controller register sequences
│       └── render.rs           # text + image rasterization (streaming)
│
├── ui/                         # layout engine (no_std, no alloc)
│   └── src/
│       ├── lib.rs
│       ├── layout.rs           # fixed-size widget tree (arena, not tree-ptr)
│       └── font.rs             # embedded bitmap fonts
│
└── proto/                      # wire protocols (no_std, no alloc)
    └── src/
        ├── epub.rs             # minimal EPUB/ZIP reader (streaming)
        └── wifi_sync.rs        # book sync protocol
```

Workspace `Cargo.toml` pins a single `[profile.release]` for all crates:
```toml
[profile.release]
opt-level = "s"          # size over speed
lto = "fat"
codegen-units = 1
panic = "abort"
```

---

## 4. Async Runtime

Embassy is the sole concurrency primitive. No RTOS, no threads.

```
                      ┌─────────────────────────────┐
                      │  embassy_executor::Executor  │
                      │  (single-core, RISC-V timer) │
                      └──────────┬──────────────────┘
           ┌──────────┬──────────┼──────────┬──────────┐
           │          │          │          │          │
       display     input      power       wifi     idle
        task        task       task        task     task
```

**Task sizing**: each task is a `static` Embassy `TaskStorage`. Stacks are embedded in the task struct — no dynamic allocation.

```rust
// fw/src/main.rs (sketch)
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = esp_hal::init(Default::default());
    spawner.spawn(tasks::display::run(p.SPI2, p.GPIO21, p.DMA_CH0)).unwrap();
    spawner.spawn(tasks::input::run(p.GPIO3)).unwrap(); // GPIO3 is home button
    spawner.spawn(tasks::power::run()).unwrap();
    spawner.spawn(tasks::wifi::run(p.WIFI)).unwrap();
}
```

**Inter-task communication**: Embassy `Channel<CriticalSectionRawMutex, T, N>` with `N` bounded at compile time. No unbounded queues.

| Channel | Type | Capacity |
|---------|------|----------|
| `UI_CMD` | `UiCommand` enum | 4 |
| `PAGE_REQ` | `PageRequest` | 2 |
| `POWER_EVT` | `PowerEvent` | 4 |

---

## 5. Display Subsystem

### EPD controller

The X4 uses an SPI-attached EPD controller (register map to be confirmed against `open-x4-epaper/community-sdk`). The driver abstraction is controller-agnostic behind a `EpdController` trait.

### Framebuffer

```rust
// display/src/fb.rs
pub struct Framebuffer {
    data: [u8; FB_BYTES],  // 48_000 bytes, 1 bpp, row-major
}

impl Framebuffer {
    /// Returns a horizontal slice (band) ready for SPI DMA.
    /// y_start..y_start+height must be within 0..480.
    pub fn band(&self, y_start: u16, height: u16) -> &[u8] {
        let row_bytes = 800 / 8;
        let start = y_start as usize * row_bytes;
        let end   = start + height as usize * row_bytes;
        &self.data[start..end]
    }
}
```

### Streaming render model

**Never** read-modify-write a full frame into a second buffer. Instead:

1. Rasterizer writes directly into `STATIC_FRAMEBUFFER` in-place.
2. Display task sends the framebuffer in horizontal bands over SPI DMA.
3. Each band transfer is `await`-ed — other tasks run during DMA.

```
 Band 0: rows   0..79   (100×80 = 8000 px = 1000 B) → SPI DMA
 Band 1: rows  80..159  → SPI DMA
 ...
 Band 5: rows 400..479  → SPI DMA
```

Band height is tunable; 80 rows = 4 KB per band fits comfortably alongside wi-fi buffers.

### Full refresh vs. partial update

| Mode | Use | Latency (estimate, measure to confirm) |
|------|-----|---------|
| Full refresh | Page turn, power-on | ~0.7–1.5 s (panel/LUT/temp dependent) |
| Partial update | UI overlays, progress | ~200–700 ms (region- and LUT-dependent) |

> Numbers above are derived from SSD1677 waveform timing (~50 Hz frame clock × ~50–80 frame LUT) and similar 4.26" panels. The X4's actual LUT may shift these meaningfully. Measure on the BUSY pin before treating these as constants.

The `display` task exposes `refresh_full()` and `refresh_partial(region: Rect)` — both streaming, both `async`.

---

## 6. Peripheral Model

All peripherals are `esp-hal` HAL types, wrapped once in `hal-ext` for ergonomics. The pattern: one `struct` owns the HAL peripheral, is `'static`, and is passed into the Embassy task that owns it for the duration of the program.

### SPI for EPD

```rust
// hal-ext/src/spi_dma.rs (sketch)
pub struct EpdSpi {
    spi: SpiDma<'static, SPI2, DmaChannel0, FullDuplex>,
    cs:  Output<'static, GPIO21>,
    dc:  Output<'static, GPIO4>,   // data/command select
    busy: Input<'static, GPIO6>,
}

impl EpdSpi {
    pub async fn send_command(&mut self, cmd: u8) { /* dc low, tx, await */ }
    pub async fn send_data(&mut self, data: &[u8]) { /* dc high, dma tx, await */ }
    pub async fn wait_busy(&mut self) { /* poll busy pin via embassy Signal */ }
}
```

### Input Model (ADC + GPIO)

The X4 saves GPIOs by using a resistor ladder for navigation buttons.

*   **Power Button**: `GPIO3` (Digital Input). Supports deep-sleep wake.
*   **Nav Buttons (Back, OK)**: `GPIO1` (ADC1).
*   **Page Buttons (Up, Down)**: `GPIO2` (ADC1).

Embassy `Adc` is used to poll these values when the device is active.

### SPI Sharing

`SPI2` is shared between the EPD and the microSD card.

| Peripheral | CS Pin |
|------------|--------|
| EPD        | GPIO21 |
| microSD    | GPIO12 |

**Invariant**: Only one CS may be low at a time. The `display` task and `storage` task must use an Embassy `Mutex` or `ExclusiveDevice` to share the `SPI2` peripheral.

Deep-sleep wake pins are configured via `esp_hal::rtc_cntl` before entering `DeepSleep`.

---

## 7. Storage & Flash Layout

ESP32-C3 flash: 4 MB standard.

```
┌─────────────────────┬─────────┬───────────────────────────────┐
│ Region              │ Size    │ Contents                      │
├─────────────────────┼─────────┼───────────────────────────────┤
│ Bootloader          │ 64 KB   │ esp-idf 2nd stage bootloader  │
│ Partition table     │ 4 KB    │                               │
│ NVS (settings)      │ 24 KB   │ user prefs, last-read pos     │
│ OTA slot 0 (fw)     │ 1.5 MB  │ active firmware               │
│ OTA slot 1          │ 1.5 MB  │ OTA staging                   │
│ LittleFS            │ ~900 KB │ books (EPUB), fonts, covers   │
└─────────────────────┴─────────┴───────────────────────────────┘
```

LittleFS (`littlefs2` crate, `no_std`) is mounted on the data partition. EPUB files are streamed directly from flash without staging them in RAM — the EPUB reader in `proto::epub` operates on a `Read + Seek` flash reader trait.

---

## 8. Power Management

Battery life is the primary non-functional constraint. The device spends >99% of its time not running.

### Sleep state machine

```
          wake interrupt
  ┌───────────────────────────────┐
  │                               ▼
DEEP_SLEEP ──────────────► BOOT / ACTIVE ──────► DISPLAY_REFRESH
  ▲                               │                      │
  │         idle_timeout          │ page rendered        │
  └───────────────────────────────┘◄─────────────────────┘
```

- **Deep sleep**: CPU off, RTC timer or GPIO wake. ~10–15 µA.
- **Light sleep**: CPU paused, Wi-Fi association kept. ~800 µA. Used during wi-fi sync window.
- **Active**: Embassy running. ~25 mA during SPI DMA refresh.

```rust
// tasks/power.rs (sketch)
async fn run() {
    loop {
        match POWER_EVT.receive().await {
            PowerEvent::PageRendered => {
                // allow display settle, then sleep
                Timer::after_millis(500).await;
                enter_deep_sleep(WAKE_GPIO_MASK);
            }
            PowerEvent::WifiSyncRequired => {
                enter_light_sleep_until(wifi_done_signal()).await;
            }
        }
    }
}
```

---

## 9. Data-Oriented Design

With 380 KB RAM and no D-cache (SRAM is single-cycle direct-access), the cost of pointer indirection is not cache misses — it is **heap fragmentation, refcount overhead, and unpredictable working set size**. The chip's ~16 KB flash cache only affects code execution from XIP flash, not data access. All hot-path data therefore uses Structure-of-Arrays (SoA) layout backed by `static` storage.

### UI widget arena

The layout engine holds a flat arena, not a tree of `Box<dyn Widget>`:

```rust
// ui/src/layout.rs
const MAX_WIDGETS: usize = 64;

pub struct Arena {
    kinds:   [WidgetKind; MAX_WIDGETS],    // enum discriminant, 1 B each
    rects:   [Rect;       MAX_WIDGETS],    // x, y, w, h — 8 B each
    texts:   [TextSlot;   MAX_WIDGETS],    // offset+len into string pool
    visible: [bool;       MAX_WIDGETS],
    count:   usize,
}
```

Iteration over `rects` for hit-testing is a single linear scan of 64 × 8 = 512 bytes — fits in a handful of cache lines.

### Font glyph tables

Bitmap fonts are stored as `&'static [u8]` in flash (`.rodata`), indexed by a sorted `&'static [(char, u16)]` glyph map (char → byte offset). Binary search, no heap.

### Page rendering pipeline

```
Flash (EPUB) ─► [streaming XML parser] ─► [line-break engine]
    ─► writes rows directly into STATIC_FRAMEBUFFER
    ─► signals display task when band N is ready
    ─► display task starts DMA for band N in parallel with rasterization of band N+1
```

This pipeline overlaps I/O and compute without any intermediate buffer beyond the one active band.

### 9.1 DOD Invariants

Rules that constrain code in firmware crates. These are not style preferences — violations create memory pressure or fragmentation that this hardware cannot absorb.

#### Banned types in `fw`, `display`, `ui`, `proto`

| Type | Why banned | Use instead |
|------|------------|-------------|
| `alloc::vec::Vec<T>` | unbounded heap | `heapless::Vec<T, N>` or `[T; N]` |
| `alloc::boxed::Box<T>` | heap | direct ownership, `&'static mut` via `StaticCell` |
| `alloc::string::String` | heap | `heapless::String<N>` or `&'static str` |
| `alloc::collections::BTreeMap/HashMap` | heap + log n | sorted `&[(K, V)]` + binary search |
| `alloc::rc::Rc` / `Arc` | refcount + heap | indices into an arena |
| `dyn Trait` *owned* (e.g. `Box<dyn T>`) | requires heap | tagged `enum` with explicit variants |
| `&'a dyn Trait` *borrowed* | OK in narrow cases | prefer `enum` for hot paths; allow at boundaries |

Enforce at the crate level:

```rust
// firmware crate roots
#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(clippy::large_stack_arrays)]
#![deny(clippy::large_types_passed_by_value)]
// alloc is simply not in the dependency graph — there's no `extern crate alloc;` anywhere.
```

The `esp-wifi` crate internally uses its own static pools — that's the one exception, and it's contained behind its API.

#### Required layouts

| Domain | Required shape |
|--------|----------------|
| Widget tree | arena with parallel arrays (`kinds`, `rects`, …) indexed by `u8` |
| Display init sequences | `const &'static [SpiOp]` — lives in flash, zero RAM cost |
| Fonts | `&'static [u8]` glyph bitmaps + sorted `&'static [(char, u16)]` index |
| Text spans | `(offset: u32, len: u16)` into a single byte slice; no nested `Vec`s |
| Render queue | fixed-size ring of an `enum BandJob` |
| Task channels | `embassy_sync::Channel<_, T, N>` with `N` const |

#### Size budgets (compile-time enforced)

```rust
// display/src/lib.rs
pub const FB_WIDTH:  usize = 800;
pub const FB_HEIGHT: usize = 480;
pub const FB_BYTES:  usize = FB_WIDTH * FB_HEIGHT / 8;        // 48_000
pub const BAND_ROWS: usize = 80;
pub const BAND_BYTES: usize = FB_WIDTH * BAND_ROWS / 8;        // 8_000

const _: () = assert!(FB_BYTES == 48_000);
const _: () = assert!(BAND_BYTES <= 16_384, "band exceeds DMA buffer budget");
```

```rust
// ui/src/layout.rs
pub const MAX_WIDGETS: usize = 64;

const _: () = assert!(
    MAX_WIDGETS * core::mem::size_of::<Rect>() < 1024,
    "widget rect array exceeds 1 KB — review MAX_WIDGETS"
);
```

If a `const _: () = assert!(...)` fires, compilation fails. Size regressions cannot land silently.

#### Indices, not pointers

Edges between arena entries are `u8` or `u16` indices, never `&T` or raw pointers. Reasons:

- Survives arena compaction without rewriting edges.
- 4× smaller than a pointer on a 32-bit target.
- "None" encodes as a sentinel (`u8::MAX`) — no `Option<&T>` overhead.

```rust
pub type WidgetId = u8;
pub const NO_WIDGET: WidgetId = u8::MAX;

pub struct Arena {
    kinds:   [WidgetKind; MAX_WIDGETS],
    parents: [WidgetId;   MAX_WIDGETS],   // NO_WIDGET = root
    // ...
}
```

#### Where ergonomic APIs are still allowed

Internal *storage* is DOD; method-shaped APIs at the boundary are fine when they improve call-site clarity:

- `fb.band(y, h) -> &[u8]` — OK. The internals are flat; the method is a view.
- `arena.draw_into(fb)` — OK. Takes a `&mut Framebuffer`, iterates flat arrays internally.
- `fn make_button() -> Box<dyn Widget>` — **banned**. Returns owned trait object.
- `fn render(w: &dyn Drawable)` — discouraged in hot paths, acceptable at module boundaries with a comment justifying why an `enum` would be worse.

#### Verification checklist (per PR)

1. `rg -nP '\b(Vec|Box|String|HashMap|BTreeMap|Rc|Arc)::' fw/ display/ ui/ proto/` returns zero hits outside comments.
2. `cargo size --release -p fw` shows `.bss + .data < 100 KB`. Diff against `main` posted in PR description.
3. No new `static mut` without `SyncUnsafeCell` or `embassy_sync::Mutex` wrapping.
4. Any new `const _: () = assert!(...)` size guard added next to the limit it protects.
5. No new crate added to the workspace `[dependencies]` graph that pulls `alloc` transitively. Verify with `cargo tree -p fw --edges normal | rg ' alloc'`.

#### What DOD does *not* mean here

- It does not mean "no methods." `impl Framebuffer { fn band(&self, …) }` is fine.
- It does not mean "no enums." Tagged unions like `SpiOp`, `WidgetKind`, `PowerEvent` are central — they're how we replace `dyn Trait` with stack-allocated variants.
- It does not mean "linear scan everything forever." Up to a few hundred entries, linear is faster than a hash on this CPU. Above that, sort + binary search. Hash maps need allocators we don't have.
- It does not mean "premature flattening." If the natural shape of a one-off, cold-path computation is a small struct passed by value, keep it. The invariants target *persistent state*, not every local variable.

---

## 10. SPI Command Encoding

Display register initialization sequences are expressed as `const` arrays of `SpiOp` — a tagged enum that is zero-cost over a raw `(u8, &[u8])` representation:

```rust
// display/src/epd.rs
#[repr(u8)]
pub enum SpiOp {
    /// Write command byte, then data bytes.
    Cmd { cmd: u8, data: &'static [u8] },
    /// Delay in milliseconds.
    DelayMs(u16),
    /// Assert/deassert reset pin.
    Reset,
}

/// Init sequence translated from C community-sdk register arrays.
/// Each entry is a safe, typed representation of a raw SPI transaction.
pub static INIT_SEQUENCE: &[SpiOp] = &[
    SpiOp::Reset,
    SpiOp::Cmd { cmd: 0x12, data: &[] }, // SW Reset
    SpiOp::DelayMs(10),
    // Driver Output Control: 480 gate lines (0x01DF), scan direction
    SpiOp::Cmd { cmd: 0x01, data: &[0xDF, 0x01, 0x00] },
    // Data Entry Mode: X increment, Y increment
    SpiOp::Cmd { cmd: 0x11, data: &[0x03] },
    // Set RAM X: 0..99 bytes (800 pixels / 8)
    SpiOp::Cmd { cmd: 0x44, data: &[0x00, 0x63] },
    // Set RAM Y: 0..479 lines
    SpiOp::Cmd { cmd: 0x45, data: &[0x00, 0x00, 0xDF, 0x01] },
    // Border Waveform Control
    SpiOp::Cmd { cmd: 0x3C, data: &[0x01] },
    // Temperature Sensor: Internal
    SpiOp::Cmd { cmd: 0x18, data: &[0x80] },
    // Display Update Control 2: Load temp, enable clock
    SpiOp::Cmd { cmd: 0x22, data: &[0xB1] },
    SpiOp::Cmd { cmd: 0x20, data: &[] }, // Master Activation
];

pub async fn run_sequence(spi: &mut EpdSpi, seq: &[SpiOp]) {
    for op in seq {
        match op {
            SpiOp::Cmd { cmd, data } => {
                spi.send_command(*cmd).await;
                if !data.is_empty() {
                    spi.send_data(data).await;
                }
            }
            SpiOp::DelayMs(ms) => Timer::after_millis(*ms as u64).await,
            SpiOp::Reset => spi.pulse_reset().await,
        }
    }
}
```

> **Note**: the register values above are placeholders. The authoritative values must be transcribed from the `open-x4-epaper/community-sdk` C source. Each `SpiOp::Cmd` corresponds to one C `epd_write_cmd(cmd, data, len)` call.

---

## 11. Build System

### Toolchain

```toml
# rust-toolchain.toml
[toolchain]
channel = "nightly-2025-10-01"   # pin for reproducibility
targets = ["riscv32imc-unknown-none-elf"]
components = ["rust-src", "clippy", "rustfmt"]
```

### Key dependencies

```toml
# fw/Cargo.toml (partial)
[dependencies]
embassy-executor  = { version = "0.6", features = ["arch-riscv32", "executor-thread"] }
embassy-time      = { version = "0.3", features = ["generic-queue-8"] }
embassy-sync      = "0.6"
esp-hal           = { version = "0.21", features = ["esp32c3", "async"] }
esp-wifi          = { version = "0.9",  features = ["esp32c3", "wifi", "async"] }
embedded-hal-async = "1.0"
littlefs2         = { version = "0.5", default-features = false }
```

### Flash & run

```sh
# install espflash once
cargo install espflash

# build + flash + monitor
cargo build --release -p fw
espflash flash --monitor target/riscv32imac-unknown-none-elf/release/fw
```

---

## 12. Open hardware questions

Items the code commits to *a value* but which need a measurement on a real
unit before we should treat them as truth. Each line names the file with the
working assumption.

| Item | Working assumption | Source / why we picked it | Confirm by |
|------|-------------------|---------------------------|------------|
| BUSY polarity on GPIO6 | active-low (LOW = busy) | `hal-ext/src/spi_dma.rs` — papyrix X4 driver doc says X4 board has an inverter | scope GPIO6 across a full refresh |
| EPD SPI clock | 40 MHz | `fw/src/main.rs` — papyrix X4 spec | logic analyzer on SCK during init |
| SSD1677 init sequence | papyrix-derived 800×480 sequence + booster soft-start + Hi-Z border | `display/src/epd.rs` — the X4 community SDK lists a 480×680 variant; we recomputed RAM-window bytes for 800×480 | flash + visually confirm the panel responds |
| Full refresh wall time | 0.7–1.5 s | §5 estimate from SSD1677 waveform timing | measure BUSY pulse width |
| ADC band thresholds for nav ladders | `input.rs` placeholders (Back: 60–500, Confirm: 700–1200, …) | guessed at uniform spacing across 0–4095 at 11 dB | log raw ADC values while pressing each button |
| Deep-sleep current | ~10–15 µA target | datasheet | uA meter on battery rail during `enter_deep_sleep_timer` |

Phase 1 exit criteria require all six rows above to move from "assumed" to "measured."

---

## Appendix: Constraint Summary

| Constraint | Value | Source |
|-----------|-------|--------|
| CPU | ESP32-C3, 160 MHz, single-core RISC-V | datasheet |
| Usable DRAM | ~380 KB | validated |
| External PSRAM | none | validated |
| Framebuffer (1 bpp, 800×480) | 48,000 B | calculated |
| Heap allocation | **forbidden** | engineering constraint |
| Async runtime | Embassy | battery / single-core constraint |
| Target triple | `riscv32imc-unknown-none-elf` | esp-rs canonical for C3 |
| Atomics | A-extension absent; `portable-atomic` with `unsafe-assume-single-core` | esp-hal feature gate |
| Flash XIP cache | ~16 KB (instruction only) | ESP32-C3 datasheet |
| Display refresh (full) | ~0.7–1.5 s (estimate) | EPD physics — measure with scope |
| Deep-sleep current | ~10–15 µA | ESP32-C3 datasheet |
