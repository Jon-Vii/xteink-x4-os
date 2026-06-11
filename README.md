# xteink-x4-os

Bare-metal Rust firmware for the Xteink X4 e-ink reader. The chip is an
ESP32-C3. The panel is an 800x480 SSD1677. There is no PSRAM.

## What it does

It reads EPUBs from the microSD card. Each book is parsed once, on the
device, into a binary cache. After that it opens fast.

The type is Literata. You can change the size and the line spacing.
Italics, bold, and blockquotes render as the book intends. Chapters
have a menu.

A page turn takes about half a second. A refresh planner decides when a
page deserves the full flash that clears ghosting, and when it does not.

When you stop reading, the chip sleeps behind a sleep screen. Your
place is saved to the card.

Progress syncs with a [kosync](https://github.com/koreader/koreader-sync-server)
server — the same protocol KOReader speaks. The radio needs more memory
than the firmware has free, so the sync session borrows the reader's
own buffers, does its work, and resets on the way out.

With no Wi-Fi credentials yet, sync raises the device's own hotspot
instead: a QR code to join it, and a form that asks for your network.

After each sync, the device serves a small shelf page. Open it in a
browser to add books or remove them.

## How it works

The firmware is a small data pipeline:

```text
buttons -> app state -> display command -> framebuffer -> SSD1677 RAM -> refresh -> sleep
```

Pure logic lives in crates that build on the host: `app-core`, `proto`,
`ui`, `display`. The `fw` crate owns what only hardware has: tasks,
pins, DMA. An emulator replays TOML scenarios through the same logic
and checks every frame against the golden images in `fixtures/golden`.

[ARCHITECTURE.md](ARCHITECTURE.md) explains the tasks, the memory, and
the refresh policy. [CONTEXT.md](CONTEXT.md) defines the words this
repo leans on.

## Building and flashing

You need the pinned nightly toolchain — rustup reads
`rust-toolchain.toml` and fetches the `riscv32imc-unknown-none-elf`
target with it — and [espflash](https://github.com/esp-rs/espflash).

```sh
cargo check --target riscv32imc-unknown-none-elf --release   # build firmware
cargo run -p fw --release                                    # flash + serial monitor
```

The kosync account is set at compile time: `XTEINK_KOSYNC_HOST` (host
or host:port, plain HTTP), `XTEINK_KOSYNC_USER`, `XTEINK_KOSYNC_PASS`.
Wi-Fi credentials come from the onboarding portal, or from
`XTEINK_WIFI_SSID`/`XTEINK_WIFI_PASS` in a dev build.

## Host tooling

The workspace's default target is the ESP32-C3, so host runs name the
host triple — `aarch64-apple-darwin` below.

```sh
cargo test -p app-core -p proto --target aarch64-apple-darwin
cargo run --manifest-path tools/emulator/Cargo.toml --target aarch64-apple-darwin --no-default-features -- --scenario fixtures/scenarios --check fixtures/golden
cargo run --manifest-path tools/emulator/Cargo.toml --target aarch64-apple-darwin --features gui -- --gui
```

The emulator's `--gui` flag drives the full UI on your desktop.
`tools/preview` renders typography with no hardware in the loop.
