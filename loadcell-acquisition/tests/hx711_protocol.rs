//! Test the firmware's actual synchronous transaction against scripted DOUT.
//! This verifies protocol ordering/delay requests, not physical GPIO timing.
use loadcell_acquisition::hx711::{ProtocolError, Recovery, SettlingWire, Wire, read_a128, reset};
use std::{
    cell::Cell,
    collections::VecDeque,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct Pins {
    data: VecDeque<bool>,
    clock_high: bool,
    time_us: u32,
    edges: Vec<(u32, bool)>,
    samples: Vec<(u32, bool)>,
    stretch_pulse: Option<usize>,
    extra_high_us: u32,
    settle_gate: Rc<Cell<bool>>,
    settle_requests: Vec<u32>,
}

impl Wire for Pins {
    fn set_clock(&mut self, high: bool) {
        assert_ne!(self.clock_high, high, "clock must alternate");
        self.clock_high = high;
        self.edges.push((self.time_us, high));
    }

    fn data_high(&mut self) -> bool {
        self.samples.push((self.time_us, self.clock_high));
        self.data.pop_front().expect("unexpected DOUT read")
    }

    fn delay_us(&mut self, us: u32) {
        self.time_us += us;
        if self.clock_high
            && self
                .stretch_pulse
                .is_some_and(|pulse| self.edges.len() == pulse * 2 - 1)
        {
            self.time_us += self.extra_high_us;
        }
    }

    fn now_us(&mut self) -> u64 {
        u64::from(self.time_us)
    }
}

impl SettlingWire for Pins {
    async fn settle_ms(&mut self, ms: u32) {
        self.settle_requests.push(ms);
        poll_fn(|_| {
            if self.settle_gate.get() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn conversion(word: u32, idle_high: bool) -> Pins {
    Pins {
        data: [false, false]
            .into_iter()
            .chain((0..24).rev().map(|bit| word & (1 << bit) != 0))
            .chain([idle_high])
            .collect(),
        ..Default::default()
    }
}

#[test]
fn a128_reads_msb_first_with_25_pulses_and_datasheet_delay_margins() {
    // Include both signed extrema, all ones (-1), zero and alternating bits.
    for word in [
        0, 0xffffff, 0x800000, 0x7fffff, 0xaaaaaa, 0x555555, 0x123456,
    ] {
        let mut pins = conversion(word, true);
        assert_eq!(read_a128(&mut pins), Ok(word));
        assert!(!pins.clock_high);
        assert!(pins.data.is_empty());
        assert_eq!(pins.edges.len(), 50);
        assert_eq!(pins.samples.len(), 27);
        assert_eq!(pins.edges[0], (2, true)); // explicit ready-to-clock delay
        for pulse in 0..25 {
            assert_eq!(pins.edges[pulse * 2], (2 + pulse as u32 * 4, true));
            assert_eq!(pins.edges[pulse * 2 + 1], (4 + pulse as u32 * 4, false));
            if pulse < 24 {
                assert_eq!(pins.samples[pulse + 2], (4 + pulse as u32 * 4, false));
            }
        }
        assert_eq!(pins.samples[26], (102, false)); // idle check after pulse 25
    }
}

#[test]
fn stale_ready_interrupt_and_short_low_glitch_do_not_clock_busy_adc() {
    for levels in [vec![true], vec![false, true]] {
        let mut pins = Pins {
            data: levels.into(),
            ..Default::default()
        };
        assert_eq!(read_a128(&mut pins), Err(ProtocolError::NotReady));
        assert!(pins.edges.is_empty());
        assert!(!pins.clock_high);
    }
}

#[test]
fn stuck_low_and_missing_conversion_handshake_are_rejected() {
    for word in [0, 0xffffff, 0x123456] {
        let mut pins = conversion(word, false);
        assert_eq!(read_a128(&mut pins), Err(ProtocolError::NotIdle));
        assert_eq!(pins.edges.len(), 50);
        assert!(!pins.clock_high);
    }
}

#[test]
fn reset_deliberately_exceeds_power_down_threshold_and_leaves_clock_low() {
    let mut pins = Pins::default();
    reset(&mut pins);
    assert_eq!(pins.edges, [(0, true), (80, false)]);
    assert_eq!(pins.time_us, 82);
    assert!(!pins.clock_high);
    assert!(pins.samples.is_empty());
}

#[test]
fn every_high_pulse_including_gain_select_is_guarded_and_aborts_low() {
    for pulse in 1..=25 {
        for extra in [48, 59, 1000] {
            let mut pins = conversion(0x123456, true);
            pins.stretch_pulse = Some(pulse);
            pins.extra_high_us = extra;
            assert_eq!(read_a128(&mut pins), Err(ProtocolError::Timing));
            assert!(!pins.clock_high);
            assert_eq!(
                pins.edges.len(),
                pulse * 2,
                "must not continue a corrupt word"
            );
        }
    }
    let mut pins = conversion(0x123456, true);
    pins.stretch_pulse = Some(5);
    pins.extra_high_us = 47;
    assert_eq!(read_a128(&mut pins), Ok(0x123456)); // measured interval 49 us
}

#[test]
fn protocol_failure_requires_reset_and_settling_before_next_word() {
    let mut pins = conversion(0, false);
    pins.settle_gate.set(true);
    let mut recovery = Recovery::default();
    assert_eq!(recovery.read_ready(&mut pins), Err(ProtocolError::NotReady));
    assert!(pins.edges.is_empty());
    assert!(poll(std::pin::pin!(recovery.prepare(&mut pins)).as_mut()).is_ready());
    assert_eq!(recovery.read_ready(&mut pins), Err(ProtocolError::NotIdle));
    let edges_before_recovery = pins.edges.len();
    pins.settle_gate.set(false);
    {
        let mut waiting = std::pin::pin!(recovery.prepare(&mut pins));
        assert!(poll(waiting.as_mut()).is_pending());
    }
    assert_eq!(pins.edges.len(), edges_before_recovery + 2);
    assert!(!pins.clock_high);
    assert_eq!(pins.settle_requests, [500, 500]);
    assert_eq!(recovery.read_ready(&mut pins), Err(ProtocolError::NotReady));

    // Cancellation must not mark settling complete or start clocking a sample.
    let edges_after_reset = pins.edges.len();
    {
        let mut waiting_again = std::pin::pin!(recovery.prepare(&mut pins));
        assert!(poll(waiting_again.as_mut()).is_pending());
    }
    assert_eq!(pins.edges.len(), edges_after_reset); // no second reset
    pins.settle_gate.set(true);
    assert!(poll(std::pin::pin!(recovery.prepare(&mut pins)).as_mut()).is_ready());
    pins.data = conversion(0xffffff, true).data;
    assert_eq!(recovery.read_ready(&mut pins), Ok(0xffffff));
}

#[test]
fn timing_failure_schedules_recovery_and_clean_reads_do_not_reset_again() {
    let mut pins = conversion(0x555555, true);
    pins.settle_gate.set(true);
    let mut recovery = Recovery::default();
    assert!(poll(std::pin::pin!(recovery.prepare(&mut pins)).as_mut()).is_ready());
    // The first two edges were the reset, so stretch the first data pulse.
    pins.stretch_pulse = Some(2);
    pins.extra_high_us = 60;
    assert_eq!(recovery.read_ready(&mut pins), Err(ProtocolError::Timing));
    pins.stretch_pulse = None;
    assert!(poll(std::pin::pin!(recovery.prepare(&mut pins)).as_mut()).is_ready());
    assert_eq!(pins.settle_requests, [500, 500]);
    pins.data = conversion(0x555555, true).data;
    assert_eq!(recovery.read_ready(&mut pins), Ok(0x555555));
    assert!(poll(std::pin::pin!(recovery.prepare(&mut pins)).as_mut()).is_ready());
    assert_eq!(pins.settle_requests.len(), 2);
    recovery.request_reset(); // a new terminal session always resets again
    assert!(poll(std::pin::pin!(recovery.prepare(&mut pins)).as_mut()).is_ready());
    assert_eq!(pins.settle_requests.len(), 3);
}

#[test]
fn one_sensor_settling_does_not_prevent_other_three_from_reading() {
    let mut pins: [Pins; 4] = std::array::from_fn(|_| conversion(123, true));
    for pin in &pins[1..] {
        pin.settle_gate.set(true);
    }
    let mut recovery: [Recovery; 4] = std::array::from_fn(|_| Recovery::default());
    {
        let [a, b, c, d] = &mut recovery;
        let [pa, pb, pc, pd] = &mut pins;
        let mut preparation = std::pin::pin!(embassy_futures::join::join4(
            a.prepare(pa),
            b.prepare(pb),
            c.prepare(pc),
            d.prepare(pd)
        ));
        assert!(poll(preparation.as_mut()).is_pending());
    }
    assert_eq!(
        recovery[0].read_ready(&mut pins[0]),
        Err(ProtocolError::NotReady)
    );
    for i in 1..4 {
        assert_eq!(recovery[i].read_ready(&mut pins[i]), Ok(123));
    }
}
