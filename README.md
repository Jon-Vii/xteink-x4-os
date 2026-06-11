# xteink-x4-os

From-scratch, bare-metal Rust firmware for the Xteink X4 e-ink reader:
an ESP32-C3 with no PSRAM driving an 800x480 SSD1677 panel. A full EPUB
reader, a typography engine, and Wi-Fi sync, fit into a microcontroller.

<!-- TODO: replace the render below with a photo of the device once one exists -->
<p align="center">
  <img src="docs/screen-reading.png" width="640" alt="A rendered book page: Literata type, styled runs, an indented blockquote">
</p>

Every screenshot in this README is a golden frame from the host
emulator — the exact bytes the panel receives, checked on every test
run, so these images cannot drift from what the firmware draws.

## What it does

- Reads EPUBs from the microSD card (`/BOOKS` and the card root),
  parsed on-device into a binary cache so books reopen instantly.
- Literata typography with adjustable type size and line spacing,
  italic/bold runs, blockquote geometry, and chapter navigation.
- Page turns in about half a second, with a refresh planner that
  decides when a full anti-ghosting flash is worth the flicker.
- Deep-sleeps the ESP32-C3 behind a visible sleep screen; reading
  position survives sleep, reset, and battery death.
- Syncs reading progress with a [kosync](https://github.com/koreader/koreader-sync-server)
  server, compatible with the KOReader ecosystem. The radio needs more
  heap than the firmware has, so the sync session dismantles the EPUB
  pipeline and loans its buffers to Wi-Fi, then resets on exit.
- Onboards Wi-Fi credentials with a captive portal: the device raises
  a hotspot and a QR code, your phone's sign-in sheet opens the form.
- Serves a shelf page after each sync: drop EPUBs onto it from any
  browser, watch upload progress, remove books.

## Screens

| | |
|---|---|
| ![Home screen](docs/screen-home.png) | ![Type settings](docs/screen-settings.png) |
| ![Wi-Fi onboarding QR](docs/screen-sync-qr.png) | ![Browser shelf serving](docs/screen-sync-serving.png) |

## How it works

The design goal is not to imitate a desktop OS. It is a small data
pipeline:

```text
buttons -> app state -> display command -> framebuffer -> SSD1677 RAM -> refresh -> sleep
```

Pure logic lives in host-testable crates (`app-core`, `proto`, `ui`,
`display`); the firmware crate (`fw`) owns tasks, pins, and DMA. A host
emulator replays TOML scenarios through the same reducer and panel
protocol model and compares output against the golden frames in
`fixtures/golden`.

- [ARCHITECTURE.md](ARCHITECTURE.md) — tasks, memory strategy, the
  Wi-Fi memory loan, refresh policy.
- [CONTEXT.md](CONTEXT.md) — glossary of load-bearing terms.
- [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) — current status and
  on-device validation notes.

## Building and flashing

Needs a nightly Rust toolchain with the `riscv32imc-unknown-none-elf`
target (see `rust-toolchain.toml`) and [espflash](https://github.com/esp-rs/espflash).

```sh
cargo check --target riscv32imc-unknown-none-elf --release   # build firmware
cargo run -p fw --release                                    # flash + serial monitor
```

The kosync account is compile-time for now: set `XTEINK_KOSYNC_HOST`
(host or host:port, plain HTTP), `XTEINK_KOSYNC_USER`, and
`XTEINK_KOSYNC_PASS` when building. Wi-Fi credentials come from the
onboarding portal, or from `XTEINK_WIFI_SSID`/`XTEINK_WIFI_PASS` for
dev builds.

## Host tooling

The workspace's default target is the ESP32-C3, so host runs name the
host triple explicitly (`aarch64-apple-darwin` below):

```sh
cargo test -p app-core -p proto --target aarch64-apple-darwin
cargo run --manifest-path tools/emulator/Cargo.toml --target aarch64-apple-darwin --no-default-features -- --scenario fixtures/scenarios --check fixtures/golden
cargo run --manifest-path tools/emulator/Cargo.toml --target aarch64-apple-darwin --features gui -- --gui
```

The emulator's `--gui` mode drives the full UI interactively on the
desktop; `tools/preview` renders typography experiments without
hardware in the loop.
