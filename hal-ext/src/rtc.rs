//! Thin wrappers around `esp_hal::rtc_cntl::Rtc` for the sleep modes the X4
//! firmware uses. All sleep transitions are *terminal* — the chip reboots on
//! wake and re-enters `main()`, so these functions return `!`.

use core::time::Duration;
use esp_hal::rtc_cntl::sleep::TimerWakeupSource;
use esp_hal::rtc_cntl::Rtc;

/// Enters deep sleep with an RTC timer wake source. The chip draws ~10–15 µA
/// during this window and reboots from `main` after `duration` elapses.
///
/// **Phase 1**: timer-only wake. Phase 2 will switch this to a GPIO3 RTC-IO
/// wake source so the user's button press is the trigger. That refactor needs
/// `GpioPin<3>` ownership moved into this crate's caller, which is gated on
/// having hardware to validate against.
pub fn enter_deep_sleep_timer(mut rtc: Rtc, duration: Duration) -> ! {
    let wakeup = TimerWakeupSource::new(duration);
    rtc.sleep_deep(&[&wakeup])
}

/// Light sleep with a short RTC timer wake. Keeps DRAM context, used during
/// Wi-Fi sync windows.
pub fn enter_light_sleep_timer(mut rtc: Rtc, duration: Duration) {
    let wakeup = TimerWakeupSource::new(duration);
    rtc.sleep_light(&[&wakeup]);
}
