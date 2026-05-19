# xteink-x4-os — Implementation Plan

This document details the step-by-step implementation plan for building the bare-metal Rust firmware for the Xteink X4 e-ink reader. It serves as our active blueprint for development, outlining the architectural boundaries, dependency graph, memory budgets, and layout strategies.

---

## Architectural & Memory Constraints

| Constraint | Value | Rationale |
|---|---|---|
| **CPU Target** | `riscv32imc-unknown-none-elf` | ESP32-C3 single-core RISC-V, no hardware FPU, no atomic A-extension |
| **Memory Limit** | ~380 KB DRAM | No external PSRAM; dynamic allocation is forbidden |
| **Concurrency** | Embassy Async Runtime | Cooperative multi-tasking without preemption or thread stack overhead |
| **Safety** | `#![forbid(unsafe_code)]` | Enforced at the application/library crate level |
| **Graphics** | Streaming 1bpp | Decoupled frame rendering using 48 KB buffer streamed over SPI DMA bands |

---

## Workspace Layout & Dependencies

The project is structured as a multi-crate cargo workspace to isolate hardware abstractions, rendering logic, and layout engines.

```
xteink-x4-os/
├── Cargo.toml                  # Workspace configuration
├── ARCHITECTURE.md             # Hardware research & registry sequences
├── IMPLEMENTATION_PLAN.md      # This document
│
├── fw/                         # Firmware binary crate (Embassy app)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # Core initialization & task spawner
│       └── tasks/
│           ├── display.rs      # SPI2 EPD DMA refresh loop
│           ├── input.rs        # ADC resistor ladder & GPIO3 polling
│           ├── power.rs        # RTC Deep Sleep state machine
│           └── wifi.rs         # esp-wifi synchronization task
│
├── hal-ext/                    # Thin ESP32-C3 async wrappers
│   └── src/
│       ├── spi_dma.rs          # Async SPI DMA helpers
│       ├── rtc.rs              # RTC / Deep Sleep controls
│       └── nvm.rs              # Key-value configuration storage
│
├── display/                    # EPD driver (no_std, zero-alloc)
│   └── src/
│       ├── lib.rs
│       ├── fb.rs               # 1bpp row-major Framebuffer
│       ├── epd.rs              # SSD1677 SPI transaction sets
│       └── render.rs           # In-place primitive & text drawing
│
├── ui/                         # Layout engine (no_std, zero-alloc, DOD)
│   └── src/
│       ├── lib.rs
│       ├── layout.rs           # Flat parallel array widget arena
│       └── font.rs             # Embedded flash bitmap font lookup
│
└── proto/                      # Streaming protocols (no_std, zero-alloc)
    └── src/
        ├── epub.rs             # Streaming ZIP / XML parser
        └── wifi_sync.rs        # Network file transfer protocol
```

---

## Technical Components Detail

### 1. Workspace Configuration
*   **Cargo.toml**: Pins a single size-optimized global release profile:
    ```toml
    [profile.release]
    opt-level = "s"          # Optimize for size
    lto = "fat"              # Full Link-Time Optimization
    codegen-units = 1
    panic = "abort"          # No stack unwinding
    ```
*   **rust-toolchain.toml**: Pins compiler `nightly-2025-10-01` and target `riscv32imc-unknown-none-elf`.
*   **.cargo/config.toml**: Configures standard compilation flags and sets `espflash` as our default runner.

### 2. display Crate
*   **Framebuffer**: Declares `[u8; 48000]` representing `800x480` pixels at 1bpp. Provides a `band(y, h)` slice method returning a reference to memory bounds for safe DMA transfers.
*   **EPD Driver**: Encodes Solomon Systech SSD1677 initialization commands via a zero-cost `SpiOp` enum (`Cmd`, `DelayMs`, `Reset`).
*   **Render**: Exposes basic shapes, inverse boxes, and font rendering, modifying the buffer directly.

### 3. ui Crate (Data-Oriented Design)
*   **Arena**: Bypasses the Rust graph lifetime problem by maintaining a parallel array representation. Widgets are indexed by simple `u8` handles rather than pointer allocations:
    ```rust
    pub struct Arena {
        kinds:   [WidgetKind; 64],
        rects:   [Rect; 64],
        parents: [u8; 64], // Parent handle; u8::MAX represents root
        visible: [bool; 64],
    }
    ```
*   **Font**: Standard 8x8 or custom bitmap fonts embedded directly in flash `.rodata`, indexed by binary-searched unicode lookup tables.

### 4. proto Crate (Streaming I/O)
*   **EPUB**: Books live on-device as standard `.epub` files (ZIP + XHTML + CSS). The reader streams compressed XHTML through a no_std deflate decoder into a streaming XML tokenizer and a typesetter that lays out one page at a time directly into the framebuffer. No heavy DOM. See `ARCHITECTURE.md §7` for the deflate-window RAM cost and §12 for the phasing.

### 5. hal-ext Crate
*   **SPI DMA**: Safely wraps `esp-hal` DMA registers to transmit frame bands in the background.
*   **RTC**: Integrates low-power sleep loops to drop the chip current down to `10–15 µA` during sleep cycles.

### 6. fw Crate
*   Coordinates the Embassy cooperative executor, spawning tasks that voluntarily yield to allow background hardware processing.

---

## Verification checklist

1.  **Crate Compiles**: `cargo check --target riscv32imc-unknown-none-elf --release` succeeds without errors.
2.  **Size Guard Checks**: Static DRAM size is audited to stay under 100 KB.
3.  **Zero-Alloc Audit**: Checking compiled crates for banned allocation libraries (`rg -nP '\b(Vec|Box|String|HashMap|BTreeMap|Rc|Arc)::'`).
4.  **Hardware Run**: Monitoring serial logs using `espflash flash --monitor`.

---

## Phased rollout

On-device EPUB is the destination, but lighting up the panel and the input loop is gated on the EPD driver, not on EPUB. Phases sized so each one is testable on hardware in isolation.

| Phase | Scope | Exit criteria |
|-------|-------|---------------|
| **1 — Bring-up** | EPD init, SPI DMA framebuffer transfer, GPIO3 home button, deep-sleep loop | Panel shows static text after wake; press button → refresh; idle → ~10 µA |
| **2 — Navigation** | ADC resistor-ladder buttons (Back/OK + PageUp/PageDown), in-RAM "book" of static strings, page index in NVS | Navigate forward/back through a hardcoded book; last page persists across reboots |
| **3 — On-device EPUB text** | Deflate decoder, ZIP central directory, streaming XHTML tokenizer, line-break typesetter | One real `.epub` (text-only) opens from LittleFS and pages cleanly |
| **4 — Minimal CSS** | Parser for ~10 properties (font-size/weight/style, text-align, margin, padding, line-height, page-break-*, display block/inline) | Same EPUB now respects bold/italic and heading sizes |
| **5 — PNG covers** | PNG decoder (reuses Phase 3 deflate) + Floyd-Steinberg dither to 1bpp; cache result in flash on book-open | Book covers render in the library view |
| **6 — Wi-Fi sync** | esp-wifi association, `proto::wifi_sync` protocol, LittleFS partition | Push an EPUB from a host over Wi-Fi, appears in library |
| **7 — JPEG (stretch)** | no_std JPEG decoder | Image-heavy EPUBs render |

Phases 1–2 are the current scope. Phase 3+ depends on phase 1 working on real hardware first.

### Performance budget (text page turn)

Estimated software cost per page turn on ESP32-C3 (160 MHz, no D-cache):

| Step | Estimate |
|------|----------|
| Deflate ~2–3 KB of XHTML | 1–3 ms |
| XML tokenize | <1 ms |
| Line-break + typeset ~1200 glyphs | <2 ms |
| SPI DMA stream 48 KB framebuffer @ 10 MHz | ~40 ms (background) |
| **EPD physics (full refresh)** | **~0.7–1.5 s** (panel/LUT/temp dependent — measure) |
| EPD physics (partial refresh) | ~200–700 ms (region- and LUT-dependent) |

> The EPD numbers are an estimate, not a measured value. SSD1677 waveforms clock at ~50 Hz; a full GC LUT is typically 50–80 frames → ~1–1.6 s. Custom LUTs (Papyrix-Reader ships one) shift this. Treat 1 s as a working assumption until we have a scope on the BUSY pin.

Software is ≤10 ms regardless. Even at the optimistic 700 ms panel refresh, software is <2% of the user-perceived latency. Text rendering is never the bottleneck. The real risks are JPEG decode (mitigated by caching dithered output at book-open) and random-jump into a multi-MB book without per-chapter deflate checkpoints — both Phase 3+ design problems.

---

## Implementation status

Source of truth lives next to the code; this table is a snapshot, updated when a phase exits.

| Area | State | Notes |
|------|-------|-------|
| Workspace + crate layout | ✅ Done | matches §3 |
| Embassy executor + task spawner | ✅ Done | `fw/src/main.rs` |
| Framebuffer + font + line/rect | ✅ Done | `display/` |
| SPI DMA `EpdSpi` wrapper | ✅ Done | `hal-ext/src/spi_dma.rs`; BUSY treated active-low per papyrix X4 docs |
| EPD SPI clock @ 40 MHz | ✅ Done | confirmed against papyrix X4 spec |
| `INIT_SEQUENCE` for SSD1677 | ⚠️ Best-guess | booster soft-start + Hi-Z border added from papyrix; RAM-window calculated for 800×480; unverified on hardware |
| GPIO3 home-button input | ✅ Done | `fw/src/tasks/input.rs` (kept as debug refresh trigger) |
| ADC nav buttons (GPIO1/GPIO2 ladders) | ⚠️ Code in place, thresholds uncalibrated | classifier table + edge detection wired; raw ADC bands are placeholders pending scope readings |
| Power: deep sleep on `PageRendered` | ⚠️ Timer wake only | `IDLE_TIMEOUT_MS=5000` → `enter_deep_sleep_timer(600s)`. Phase 2 swaps to GPIO3 RTC-IO wake. |
| `Arena::draw_into` text rendering | ✅ Done | font glyphs + clipping wired |
| `RefreshPartial` / `UpdateProgressBar` | ❌ Stub | display task accepts them, does nothing |
| `hal_ext::nvm` | ❌ Emulation | returns `(i & 0xFF)` — not real flash |
| LittleFS integration | ❌ Missing | Phase 6 |
| `esp-wifi` association | ❌ Missing | wifi task is a 30 s sleep loop |
| Deflate / EPUB / CSS / images | ❌ Missing | Phase 3+ |
| `unsafe_code` lint at app level | ✅ Done | `deny(unsafe_code)` + one localized `allow` on the ESP app descriptor |
