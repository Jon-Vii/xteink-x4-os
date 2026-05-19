#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
// Deny rather than forbid so the ESP app descriptor below (which needs
// #[no_mangle] + #[link_section]) can opt-out with a localized #[allow].
// Everything else in this binary stays unsafe-free.
#![deny(unsafe_code)]
#![deny(clippy::large_stack_arrays)]
#![deny(clippy::large_types_passed_by_value)]

#[repr(C)]
pub struct EspAppDesc {
    pub magic_word: u32,
    pub secure_version: u32,
    pub reserv1: [u32; 2],
    pub version: [u8; 32],
    pub project_name: [u8; 32],
    pub time: [u8; 16],
    pub date: [u8; 16],
    pub idf_ver: [u8; 32],
    pub app_elf_sha256: [u8; 32],
    pub min_efuse_blk_rev_full: u16,
    pub max_efuse_blk_rev_full: u16,
    pub mmu_page_size: u8,
    pub spi_flash_mode: u8,
    pub reserv3: [u8; 2],
    pub reserv2: [u32; 18],
}

#[allow(unsafe_code)] // ESP-IDF bootloader looks up this symbol by name in this section.
#[link_section = ".rodata_desc"]
#[used]
#[no_mangle]
pub static _esp_app_desc: EspAppDesc = EspAppDesc {
    magic_word: 0xABCD5432,
    secure_version: 0,
    reserv1: [0; 2],
    version: *b"0.1.0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    project_name: *b"xteink-x4-os\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    time: *b"00:00:00\0\0\0\0\0\0\0\0",
    date: *b"2026-05-19\0\0\0\0\0\0",
    idf_ver: *b"5.5.1\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    app_elf_sha256: [0; 32],
    min_efuse_blk_rev_full: 0,
    max_efuse_blk_rev_full: 65535,
    mmu_page_size: 16, // 64KB log base 2
    spi_flash_mode: 2, // DIO
    reserv3: [0; 2],
    reserv2: [0; 18],
};

use embassy_executor::Spawner;
use esp_hal_embassy::Executor;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::gpio::{Io, Input, Output, Level, Pull};
use esp_hal::entry;
use esp_hal::timer::timg::TimerGroup;
use static_cell::StaticCell;
use esp_hal::prelude::*;
use esp_hal::dma::{Dma, DmaPriority};
use esp_hal::spi::master::Spi;

// Define workspace modules
pub mod tasks;

// Define task communication commands
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiCommand {
    RefreshFull,
    RefreshPartial { rect: ui::layout::Rect },
    UpdateProgressBar { percent: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageRequest {
    NextPage,
    PrevPage,
    GoToChapter { num: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    PageRendered,
    WifiSyncRequired,
    GoToSleep,
    WakeUp,
}

// Bounded compile-time channels as defined in ARCHITECTURE.md Section 4
pub static UI_CMD: Channel<CriticalSectionRawMutex, UiCommand, 4> = Channel::new();
pub static PAGE_REQ: Channel<CriticalSectionRawMutex, PageRequest, 2> = Channel::new();
pub static POWER_EVT: Channel<CriticalSectionRawMutex, PowerEvent, 4> = Channel::new();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("{}", info);
    loop {
        // Safe lockup on panic
    }
}

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

#[entry]
fn main() -> ! {
    // 1. Initialize ESP32-C3 hardware clocks & registers
    let peripherals = esp_hal::init(esp_hal::Config::default());

    esp_println::println!("--- Xteink X4 OS Booting ---");

    // Initialize Embassy time driver
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_hal_embassy::init(timg0.timer0);

    esp_println::println!("Embassy executor initialized successfully!");

    // 2. Set up GPIO pins via esp-hal Io
    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    let cs = Output::new(io.pins.gpio21, Level::High);
    let dc = Output::new(io.pins.gpio4, Level::Low);
    let rst = Output::new(io.pins.gpio5, Level::High);
    let busy = Input::new(io.pins.gpio6, Pull::None);
    let home_button = Input::new(io.pins.gpio3, Pull::Up);

    // ADC1 + GPIO1/GPIO2 ladders for nav buttons (papyrix X4 device-spec).
    // 11 dB attenuation gives full 0..~3.3 V range across the resistor ladder.
    let mut adc_cfg = AdcConfig::new();
    let nav_pin = adc_cfg.enable_pin(io.pins.gpio1, Attenuation::Attenuation11dB);
    let page_pin = adc_cfg.enable_pin(io.pins.gpio2, Attenuation::Attenuation11dB);
    let adc1 = Adc::new(peripherals.ADC1, adc_cfg);

    esp_println::println!("Hardware IO, EPD control pins, and ADC1 configured.");

    // 3. Configure DMA and SPI for EPD
    let dma = Dma::new(peripherals.DMA);
    let dma_channel = dma.channel0.configure_for_async(
        false,
        DmaPriority::Priority0,
    );

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = esp_hal::dma_buffers!(8000);
    let dma_rx_buf = esp_hal::dma::DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
    let dma_tx_buf = esp_hal::dma::DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();

    // 40 MHz EPD SPI clock per papyrix X4 docs. SSD1677 tolerates this and it
    // cuts the 48 KB framebuffer wire time from ~38 ms to ~10 ms, leaving more
    // headroom for partial-update overlays.
    let spi = Spi::new(
        peripherals.SPI2,
        40_u32.MHz(),
        esp_hal::spi::SpiMode::Mode0,
    )
    .with_sck(io.pins.gpio8)
    .with_mosi(io.pins.gpio10)
    .with_dma(dma_channel)
    .with_buffers(dma_rx_buf, dma_tx_buf);

    let epd_spi = hal_ext::spi_dma::EpdSpi::new(spi, cs, dc, busy, rst);

    // 4. Spawn tasks in parallel under Embassy
    let executor = EXECUTOR.init(Executor::new());
    esp_println::println!("Spawning system tasks...");
    executor.run(|spawner: Spawner| {
        spawner.spawn(tasks::display::run(epd_spi)).unwrap();
        spawner
            .spawn(tasks::input::run(home_button, adc1, nav_pin, page_pin))
            .unwrap();
        spawner.spawn(tasks::power::run(peripherals.LPWR)).unwrap();
        spawner.spawn(tasks::wifi::run(peripherals.WIFI)).unwrap();
    })
}
