//! Blocking GPIO clock with asynchronous data-ready wait.
//! A single 25-pulse transaction always selects HX711 channel A, gain 128.
use embassy_rp::gpio::{Input, Output};
use embassy_time::{Duration, Instant, Timer, block_for, with_timeout};
use loadcell_acquisition::Source;
use loadcell_acquisition::hx711::{Recovery, SettlingWire, Wire};
use loadcell_core::decode_24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// DOUT did not become low within the specified deadline.
    Timeout,
    /// The signed 24-bit output hit one of the ADC saturation codes.
    Saturated,
    /// Invalid DOUT handshake or excessive PD_SCK HIGH duration.
    Protocol,
}

pub struct Hx711<'d> {
    pins: Pins<'d>,
    recovery: Recovery,
}

struct Pins<'d> {
    dout: Input<'d>,
    clk: Output<'d>,
}

impl Source for Hx711<'_> {
    type Error = ReadError;

    async fn read(&mut self) -> Result<i32, ReadError> {
        // Per-sensor pacing prevents a stuck-low input from spinning without
        // sleeping. Healthy HX711s still wait for their own conversion below.
        Timer::after_millis(1).await;
        Hx711::read(self).await
    }
}

impl<'d> Hx711<'d> {
    pub fn new(dout: Input<'d>, clk: Output<'d>) -> Self {
        Self {
            pins: Pins { dout, clk },
            recovery: Recovery::default(),
        }
    }

    pub fn reset_before_next_read(&mut self) {
        self.recovery.request_reset();
    }

    pub async fn read(&mut self) -> Result<i32, ReadError> {
        // Only this device waits; peers and USB continue. A cancelled settling
        // wait remains pending in recovery, so no unsettled sample is accepted.
        self.recovery.prepare(&mut self.pins).await;
        // DOUT=low signals a new conversion. A missing device must not
        // stall the entire firmware indefinitely.
        with_timeout(Duration::from_millis(300), self.pins.dout.wait_for_low())
            .await
            .map_err(|_| ReadError::Timeout)?;

        // Normal PD_SCK HIGH must stay <=50 us; >60 us causes power-down.
        // No await or maskable interrupt can stretch a HIGH pulse. Physical
        // timing (including GPIO/translator delays) still needs measurement.
        let bits = cortex_m::interrupt::free(|_| self.recovery.read_ready(&mut self.pins))
            .map_err(|_| ReadError::Protocol)?;

        let value = decode_24(bits);
        match value {
            -8_388_608 | 8_388_607 => Err(ReadError::Saturated),
            _ => Ok(value),
        }
    }
}

impl Wire for Pins<'_> {
    fn set_clock(&mut self, high: bool) {
        if high {
            self.clk.set_high();
        } else {
            self.clk.set_low();
        }
    }

    fn data_high(&mut self) -> bool {
        self.dout.is_high()
    }

    fn delay_us(&mut self, us: u32) {
        block_for(Duration::from_micros(u64::from(us)));
    }

    fn now_us(&mut self) -> u64 {
        Instant::now().as_micros()
    }
}

impl SettlingWire for Pins<'_> {
    async fn settle_ms(&mut self, ms: u32) {
        Timer::after_millis(u64::from(ms)).await;
    }
}
