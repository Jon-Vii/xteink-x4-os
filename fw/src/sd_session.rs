use crate::display_flush::Epd;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::OutputPin;
use embedded_hal::spi::{Operation, SpiBus as BlockingSpiBus, SpiDevice};
use embedded_sdmmc::{Directory, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::prelude::*;
use esp_hal::spi::master::SpiDmaBus;
use esp_hal::spi::FullDuplexMode;
use esp_hal::Async;

pub(crate) struct StaticTime;

impl TimeSource for StaticTime {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 56,
            zero_indexed_month: 4,
            zero_indexed_day: 19,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

pub(crate) struct SdSpiDevice<'a, SPI, CS> {
    pub(crate) spi: &'a mut SPI,
    pub(crate) cs: &'a mut CS,
    pub(crate) delay: esp_hal::delay::Delay,
}

impl<SPI, CS> embedded_hal::spi::ErrorType for SdSpiDevice<'_, SPI, CS>
where
    SPI: embedded_hal::spi::ErrorType,
{
    type Error = SPI::Error;
}

impl<SPI, CS> SpiDevice for SdSpiDevice<'_, SPI, CS>
where
    SPI: BlockingSpiBus<u8>,
    CS: OutputPin,
{
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
        let _ = self.cs.set_low();
        let mut result = Ok(());

        for operation in operations {
            result = match operation {
                Operation::Read(buffer) => self.spi.read(buffer),
                Operation::Write(buffer) => self.spi.write(buffer),
                Operation::Transfer(read, write) => self.spi.transfer(read, write),
                Operation::TransferInPlace(buffer) => self.spi.transfer_in_place(buffer),
                Operation::DelayNs(ns) => {
                    self.delay.delay_ns(*ns);
                    Ok(())
                }
            };

            if result.is_err() {
                break;
            }
        }

        let _ = self.spi.flush();
        let _ = self.cs.set_high();
        result
    }
}

type SdSpi<'a> = SdSpiDevice<'a, SpiDmaBus<'static, SPI2, FullDuplexMode, Async>, Output<'static>>;
type SdCardDevice<'a> = SdCard<SdSpi<'a>, esp_hal::delay::Delay>;
pub(crate) type SdRoot<'a> = Directory<'a, SdCardDevice<'a>, StaticTime, 4, 4, 1>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SdSessionError {
    StartupClocks,
    CardInit,
    Volume,
    Root,
}

pub(crate) fn with_root<R>(
    epd: &mut Epd,
    sd_cs: &mut Output<'static>,
    f: impl for<'a> FnOnce(&SdRoot<'a>) -> R,
) -> Result<R, SdSessionError> {
    epd.deselect_display();
    sd_cs.set_high();
    epd.spi_mut().change_bus_frequency(400_u32.kHz());

    let startup_clocks = [0xFF; 10];
    if BlockingSpiBus::write(epd.spi_mut(), &startup_clocks).is_err() {
        epd.spi_mut().change_bus_frequency(40_u32.MHz());
        return Err(SdSessionError::StartupClocks);
    }

    let result = {
        let spi = SdSpiDevice {
            spi: epd.spi_mut(),
            cs: sd_cs,
            delay: esp_hal::delay::Delay::new(),
        };
        let card = SdCard::new(spi, esp_hal::delay::Delay::new());
        if card.num_bytes().is_err() {
            Err(SdSessionError::CardInit)
        } else {
            card.spi(|device| device.spi.change_bus_frequency(8_u32.MHz()));
            let volume_mgr: VolumeManager<_, _, 4, 4, 1> = VolumeManager::new(card, StaticTime);
            let result = match volume_mgr.open_volume(VolumeIdx(0)) {
                Ok(volume) => {
                    let raw_volume = volume.to_raw_volume();
                    if let Ok(raw_root) = volume_mgr.open_root_dir(raw_volume) {
                        let root = Directory::new(raw_root, &volume_mgr);
                        let value = f(&root);
                        drop(root);
                        let _ = volume_mgr.close_volume(raw_volume);
                        Ok(value)
                    } else {
                        let _ = volume_mgr.close_volume(raw_volume);
                        Err(SdSessionError::Root)
                    }
                }
                Err(_) => Err(SdSessionError::Volume),
            };
            result
        }
    };

    epd.spi_mut().change_bus_frequency(40_u32.MHz());
    result
}
