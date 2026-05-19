#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpiOp {
    /// Write command byte, then data bytes.
    Cmd { cmd: u8, data: &'static [u8] },
    /// Delay in milliseconds.
    DelayMs(u16),
    /// Assert/deassert reset pin.
    Reset,
}

/// Initialization sequence for the Xteink X4 (Good Display 4.26" panel,
/// SSD1677 controller, 800x480, 1bpp).
///
/// Values cross-referenced against the papyrix-reader SSD1677 driver doc and
/// the device-specifications doc. The booster soft-start (`0x0C`) and Hi-Z
/// border (`0x3C 0xC0`) come from there; the RAM-window bytes are calculated
/// for 800x480 (RAM X end = 99 bytes, RAM Y end = 479 lines).
///
/// **Unverified on hardware.** Differences from papyrix's example sequence:
///   - they configured the controller for a 480x680 variant; we configure 800x480.
///   - they include `0x46`/`0x47` auto-write commands for the RED/BW RAMs of a
///     dual-color panel; X4 is mono so we skip those.
/// Treat this whole block as "best guess, awaits scope-on-BUSY confirmation."
pub static INIT_SEQUENCE: &[SpiOp] = &[
    SpiOp::Reset,
    SpiOp::Cmd { cmd: 0x12, data: &[] }, // SW Reset
    SpiOp::DelayMs(10),
    // Booster Soft Start (per papyrix X4 driver doc).
    SpiOp::Cmd { cmd: 0x0C, data: &[0xAE, 0xC7, 0xC3, 0xC0, 0x40] },
    // Temperature Sensor: use internal sensor.
    SpiOp::Cmd { cmd: 0x18, data: &[0x80] },
    // Driver Output Control: 480 gate lines (MUX = 479 = 0x01DF), scan direction.
    SpiOp::Cmd { cmd: 0x01, data: &[0xDF, 0x01, 0x00] },
    // Border Waveform Control: Hi-Z (0xC0) — reduces ghosting per papyrix.
    SpiOp::Cmd { cmd: 0x3C, data: &[0xC0] },
    // Data Entry Mode: X increment, Y increment.
    SpiOp::Cmd { cmd: 0x11, data: &[0x03] },
    // Set RAM X window: 0..99 bytes (800 pixels / 8).
    SpiOp::Cmd { cmd: 0x44, data: &[0x00, 0x63] },
    // Set RAM Y window: 0..479 lines.
    SpiOp::Cmd { cmd: 0x45, data: &[0x00, 0x00, 0xDF, 0x01] },
    // Display Update Control 2: load temp, enable clock and analog.
    SpiOp::Cmd { cmd: 0x22, data: &[0xB1] },
    // Master Activation: kick off the load.
    SpiOp::Cmd { cmd: 0x20, data: &[] },
];
