//! Hardware-independent HX711 wire transaction and asynchronous recovery.
//! See HX711 datasheet pp. 4-5, Fig. 2 and Fig. 3.

pub trait Wire {
    fn set_clock(&mut self, high: bool);
    fn data_high(&mut self) -> bool;
    fn delay_us(&mut self, us: u32);
    /// Monotonic microseconds; resolution must be <=1 us.
    fn now_us(&mut self) -> u64;
}

pub trait SettlingWire: Wire {
    fn settle_ms(&mut self, ms: u32) -> impl core::future::Future<Output = ()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    NotReady,
    NotIdle,
    Timing,
}

/// Per-device recovery state survives cancellation of an asynchronous read.
#[derive(Default)]
pub enum Recovery {
    #[default]
    ResetNeeded,
    Settling,
    Ready,
}

impl Recovery {
    pub fn request_reset(&mut self) {
        *self = Self::ResetNeeded;
    }

    pub async fn prepare(&mut self, wire: &mut impl SettlingWire) {
        match self {
            Self::ResetNeeded => {
                reset(wire);
                *self = Self::Settling;
            }
            Self::Settling => {}
            Self::Ready => return,
        }
        // Datasheet settling at 10 SPS is 400 ms. If cancelled, keep the
        // Settling state and conservatively wait a full 500 ms next time.
        wire.settle_ms(500).await;
        *self = Self::Ready;
    }

    /// Called with interrupts masked, after prepare and the DOUT-ready wait.
    pub fn read_ready(&mut self, wire: &mut impl Wire) -> Result<u32, ProtocolError> {
        match self {
            Self::Ready => {}
            Self::ResetNeeded | Self::Settling => return Err(ProtocolError::NotReady),
        }
        let result = read_a128(wire);
        if result.is_err() {
            self.request_reset();
        }
        result
    }
}

fn pulse(wire: &mut impl Wire) -> Result<(), ProtocolError> {
    let start = wire.now_us();
    wire.set_clock(true);
    wire.delay_us(2);
    wire.set_clock(false);
    let elapsed = wire.now_us().saturating_sub(start);
    // Include GPIO/software overhead in the measured interval. Reject at
    // 50 us conservatively for timer quantization. This detects an overrun;
    // it cannot prevent one or certify the signal after the level shifter.
    if elapsed >= 50 {
        return Err(ProtocolError::Timing);
    }
    Ok(())
}

/// Caller must prevent preemption throughout the transaction so that each
/// PD_SCK HIGH stays below 50 us. PD_SCK must be LOW on entry and is LOW on exit.
/// An interrupt notification alone is not proof that DOUT is still ready.
pub fn read_a128(wire: &mut impl Wire) -> Result<u32, ProtocolError> {
    if wire.data_high() {
        return Err(ProtocolError::NotReady);
    }
    // Explicit T1 margin (>=0.1 us), also rejecting a transient ready-low.
    wire.delay_us(2);
    if wire.data_high() {
        return Err(ProtocolError::NotReady);
    }
    let mut word = 0_u32;
    for _ in 0..24 {
        pulse(wire)?;
        // Data changes on the rising edge; sample while LOW, as in the
        // manufacturer's reference driver, keeping work out of the HIGH phase.
        word = (word << 1) | u32::from(wire.data_high());
        wire.delay_us(2); // T4 >=0.2 us.
    }
    // Pulse 25 selects channel A / gain 128 for the next conversion and
    // must restore DOUT HIGH, irrespective of the 24-bit sample value.
    pulse(wire)?;
    wire.delay_us(2);
    if !wire.data_high() {
        return Err(ProtocolError::NotIdle);
    }
    Ok(word)
}

/// Deliberate power-down (>60 us HIGH), then reset to A / gain 128.
/// Synchronous so cancellation cannot leave PD_SCK HIGH. Caller must wait for
/// settling afterwards (400 ms at RATE=0) before using samples for calibration.
pub fn reset(wire: &mut impl Wire) {
    wire.set_clock(true);
    wire.delay_us(80);
    wire.set_clock(false);
    wire.delay_us(2);
}
