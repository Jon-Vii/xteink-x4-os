use crate::{PAGE_REQ, PageRequest, UI_CMD, UiCommand};
use embassy_futures::select::{select3, Either3};
use embassy_time::Timer;
use esp_hal::analog::adc::{Adc, AdcPin};
use esp_hal::gpio::{GpioPin, Input};
use esp_hal::peripherals::ADC1;

/// User-facing navigation events decoded from the resistor-ladder buttons.
///
/// DOD note: the polling task does not allocate or build per-press structs —
/// each tick produces `Option<NavEvent>` by linear-scanning a `const` lookup
/// table. The whole input pipeline is `Copy` types and flat arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavEvent {
    Home,
    Back,
    Confirm,
    Left,
    Right,
    Up,
    Down,
}

/// One entry of an ADC resistor-ladder lookup table: `[min, max]` raw ADC
/// reading inclusive that maps to this event.
///
/// **Calibration**: these thresholds are placeholders. The ESP32-C3 ADC at
/// 11 dB attenuation reads 0..~4095 across roughly 0..3.3 V. Real X4 ladder
/// resistor values are not in the papyrix docs, so each band below is a
/// guess that needs scope-and-log calibration on real hardware.
#[derive(Clone, Copy)]
struct LadderBand {
    min: u16,
    max: u16,
    event: NavEvent,
}

const fn band(min: u16, max: u16, event: NavEvent) -> LadderBand {
    LadderBand { min, max, event }
}

/// GPIO1 ladder: Back, Confirm, Left, Right (per papyrix X4 device-spec doc).
/// Order: low ADC reading = pressed-first-in-ladder.
const NAV_LADDER: &[LadderBand] = &[
    band(60, 500, NavEvent::Back),
    band(700, 1200, NavEvent::Confirm),
    band(1400, 2000, NavEvent::Left),
    band(2200, 2900, NavEvent::Right),
    // 3300..4095 = idle (rail). No band; classifier returns None.
];

/// GPIO2 ladder: Up, Down.
const PAGE_LADDER: &[LadderBand] = &[
    band(60, 700, NavEvent::Up),
    band(1200, 2400, NavEvent::Down),
    // 3000..4095 = idle.
];

const POLL_INTERVAL_MS: u64 = 40;

fn classify(reading: u16, table: &[LadderBand]) -> Option<NavEvent> {
    let mut i = 0;
    while i < table.len() {
        let b = table[i];
        if reading >= b.min && reading <= b.max {
            return Some(b.event);
        }
        i += 1;
    }
    None
}

/// Maps a decoded nav event to outgoing channel messages. Kept centralized so
/// the policy ("Up = previous page", "Confirm = full refresh", etc.) lives in
/// one place instead of scattered across the task.
fn dispatch(event: NavEvent) {
    match event {
        NavEvent::Up | NavEvent::Left => {
            let _ = PAGE_REQ.try_send(PageRequest::PrevPage);
            let _ = UI_CMD.try_send(UiCommand::RefreshFull);
        }
        NavEvent::Down | NavEvent::Right | NavEvent::Home => {
            let _ = PAGE_REQ.try_send(PageRequest::NextPage);
            let _ = UI_CMD.try_send(UiCommand::RefreshFull);
        }
        NavEvent::Confirm => {
            let _ = UI_CMD.try_send(UiCommand::RefreshFull);
        }
        NavEvent::Back => {
            // Phase 2: bubble up to a menu state machine.
        }
    }
}

#[embassy_executor::task]
pub async fn run(
    mut home_button: Input<'static>,
    mut adc1: Adc<'static, ADC1>,
    mut nav_pin: AdcPin<GpioPin<1>, ADC1>,
    mut page_pin: AdcPin<GpioPin<2>, ADC1>,
) {
    esp_println::println!("Input task started (GPIO3 home + ADC1 ladders on GPIO1/GPIO2).");

    let mut last_nav: Option<NavEvent> = None;
    let mut last_page: Option<NavEvent> = None;

    loop {
        // Wait for either a falling edge on GPIO3 OR the poll interval expiring.
        // The poll interval also debounces edge-detected ADC events.
        match select3(
            home_button.wait_for_falling_edge(),
            Timer::after_millis(POLL_INTERVAL_MS),
            embassy_futures::yield_now(),
        )
        .await
        {
            Either3::First(_) => {
                dispatch(NavEvent::Home);
                Timer::after_millis(200).await; // debounce
            }
            Either3::Second(_) => {
                let nav_raw = read_adc_oneshot(&mut adc1, &mut nav_pin).await;
                let page_raw = read_adc_oneshot(&mut adc1, &mut page_pin).await;

                let nav_evt = classify(nav_raw, NAV_LADDER);
                let page_evt = classify(page_raw, PAGE_LADDER);

                // Edge detect: fire only on transition from None → Some(event)
                // or between different events. Holding a button does not retrigger.
                if nav_evt != last_nav {
                    if let Some(evt) = nav_evt {
                        dispatch(evt);
                    }
                    last_nav = nav_evt;
                }
                if page_evt != last_page {
                    if let Some(evt) = page_evt {
                        dispatch(evt);
                    }
                    last_page = page_evt;
                }
            }
            Either3::Third(_) => {
                // yield_now resolves immediately; included so the future set
                // is non-empty even if the timer arm somehow stalls.
            }
        }
    }
}

/// One-shot ADC read that yields back to the executor while waiting instead of
/// busy-looping. The actual conversion is microseconds; the WouldBlock retries
/// only happen if a parallel read on the same ADC is in flight.
async fn read_adc_oneshot<P>(adc: &mut Adc<'static, ADC1>, pin: &mut AdcPin<P, ADC1>) -> u16
where
    P: esp_hal::analog::adc::AdcChannel,
{
    loop {
        match adc.read_oneshot(pin) {
            Ok(v) => return v,
            Err(nb::Error::WouldBlock) => Timer::after_micros(50).await,
            Err(_) => return 0,
        }
    }
}
