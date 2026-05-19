use embassy_time::Timer;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::spi::SpiBus;

pub struct EpdSpi<SPI, CS, DC, BUSY, RST> {
    pub spi: SPI,
    pub cs: CS,
    pub dc: DC,
    pub busy: BUSY,
    pub rst: RST,
}

impl<SPI, CS, DC, BUSY, RST> EpdSpi<SPI, CS, DC, BUSY, RST>
where
    SPI: SpiBus,
    CS: OutputPin,
    DC: OutputPin,
    BUSY: InputPin,
    RST: OutputPin,
{
    pub fn new(spi: SPI, cs: CS, dc: DC, busy: BUSY, rst: RST) -> Self {
        Self {
            spi,
            cs,
            dc,
            busy,
            rst,
        }
    }

    /// Sends a single command byte.
    pub async fn send_command(&mut self, cmd: u8) -> Result<(), SPI::Error> {
        let _ = self.dc.set_low();
        let _ = self.cs.set_low();
        let res = self.spi.write(&[cmd]).await;
        let _ = self.cs.set_high();
        res
    }

    /// Sends multiple data bytes (usually via DMA background transfer).
    pub async fn send_data(&mut self, data: &[u8]) -> Result<(), SPI::Error> {
        let _ = self.dc.set_high();
        let _ = self.cs.set_low();
        let res = self.spi.write(data).await;
        let _ = self.cs.set_high();
        res
    }

    /// Pulses EPD hardware reset pin.
    pub async fn pulse_reset(&mut self) {
        let _ = self.rst.set_low();
        Timer::after_millis(2).await;
        let _ = self.rst.set_high();
        Timer::after_millis(20).await;
    }

    /// Waits asynchronously while the display BUSY pin signals "busy".
    ///
    /// On the Xteink X4 the BUSY line is active-low (LOW = busy), per the
    /// papyrix-reader X4 spec doc. This is opposite to the bare SSD1677
    /// datasheet polarity; the board has an inverter on this line.
    pub async fn wait_busy(&mut self) {
        while self.busy.is_low().unwrap_or(false) {
            Timer::after_millis(10).await;
        }
    }
}
