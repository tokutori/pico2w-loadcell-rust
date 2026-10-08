//! Blocking GPIO clock with asynchronous data-ready wait.
//! A single 25-pulse transaction always selects HX711 channel A, gain 128.
use embassy_rp::gpio::{Input, Output};
use embassy_time::{Duration, block_for, with_timeout};
use loadcell_core::decode_24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// DOUT did not become low within the specified deadline.
    Timeout,
    /// The signed 24-bit output hit one of the ADC saturation codes.
    Saturated,
}

pub struct Hx711<'d> {
    dout: Input<'d>,
    clk: Output<'d>,
}

impl<'d> Hx711<'d> {
    pub fn new(dout: Input<'d>, clk: Output<'d>) -> Self {
        Self { dout, clk }
    }

    pub async fn read(&mut self) -> Result<i32, ReadError> {
        // DOUT=low signals a new conversion. A missing device must not
        // stall the entire firmware indefinitely.
        with_timeout(Duration::from_millis(300), self.dout.wait_for_low())
            .await
            .map_err(|_| ReadError::Timeout)?;

        // PD_SCK >60 us HIGH causes HX711 power-down. Clock the word
        // with interrupts masked for about 100 us to bound the HIGH time.
        let bits: u32 = cortex_m::interrupt::free(|_| {
            let mut word: u32 = 0;
            for _ in 0..24 {
                self.clk.set_high();
                block_for(Duration::from_micros(2));
                word = (word << 1) | u32::from(self.dout.is_high());
                self.clk.set_low();
                block_for(Duration::from_micros(2));
            }
            // 25th pulse: select next conversion as channel A, gain 128.
            self.clk.set_high();
            block_for(Duration::from_micros(2));
            self.clk.set_low();
            word
        });

        let value = decode_24(bits);
        match value {
            -8_388_608 | 8_388_607 => Err(ReadError::Saturated),
            _ => Ok(value),
        }
    }
}
