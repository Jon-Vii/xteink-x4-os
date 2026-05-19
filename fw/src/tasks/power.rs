use crate::{POWER_EVT, PowerEvent};
use core::time::Duration;
use embassy_futures::select::{select, Either};
use embassy_time::Timer;
use esp_hal::peripherals::LPWR;
use esp_hal::rtc_cntl::Rtc;

/// How long to stay awake after the display has settled. The display task
/// posts `PowerEvent::PageRendered` once a refresh completes; if no other
/// event arrives within this window we deep-sleep.
const IDLE_TIMEOUT_MS: u64 = 5_000;

/// How long to sleep before the RTC timer wakes us. Long-ish so the device
/// behaves like an e-reader (sleep until the user wants to do something)
/// without being indefinite (Phase-1 GPIO wake is not wired yet).
const DEEP_SLEEP_SECS: u64 = 600;

#[embassy_executor::task]
pub async fn run(lpwr: LPWR) {
    esp_println::println!("Power management task started.");
    let rtc = Rtc::new(lpwr);

    loop {
        match POWER_EVT.receive().await {
            PowerEvent::PageRendered => {
                // Stay responsive for IDLE_TIMEOUT_MS; if another power event
                // arrives in that window (e.g. the user kept pressing buttons),
                // restart the idle clock.
                let kept_active = wait_for_idle(IDLE_TIMEOUT_MS).await;
                if !kept_active {
                    esp_println::println!(
                        "Idle for {} ms, entering deep sleep ({} s)…",
                        IDLE_TIMEOUT_MS,
                        DEEP_SLEEP_SECS
                    );
                    hal_ext::rtc::enter_deep_sleep_timer(
                        rtc,
                        Duration::from_secs(DEEP_SLEEP_SECS),
                    );
                }
            }
            PowerEvent::WifiSyncRequired => {
                // Phase-6 work — keep the chip in light sleep while wi-fi
                // runs. For now this just yields.
                Timer::after_millis(50).await;
            }
            PowerEvent::GoToSleep | PowerEvent::WakeUp => {
                // Reserved for explicit transitions; Phase-1 uses
                // PageRendered + idle timeout as the only sleep trigger.
            }
        }
    }
}

/// Returns `true` if another `PowerEvent` arrived inside the window (the user
/// kept the device busy), `false` if the window expired without activity.
async fn wait_for_idle(idle_ms: u64) -> bool {
    match select(
        Timer::after_millis(idle_ms),
        POWER_EVT.receive(),
    )
    .await
    {
        Either::First(_) => false,
        Either::Second(_) => true,
    }
}
