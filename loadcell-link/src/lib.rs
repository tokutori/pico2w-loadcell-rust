#![no_std]
//! Application BLE framing v1. GATT UUIDs distinguish this from plain UART.
//! Each notification: LE sequence u16, flags (bit 0 start, bit 1 end), text.
//! Whole writes are delivered atomically; a lost fragment invalidates its write.
pub const SERVICE: u128 = 0x854b6df0_3908_4ae5_8862_89c122e17401;
pub const TX: u128 = 0x854b6df1_3908_4ae5_8862_89c122e17401;
pub const MAX_PACKET: usize = 125;
pub const MAX_MESSAGE: usize = 512;

#[derive(Default)]
pub struct Decoder {
    next: Option<u16>,
    partial: heapless::Vec<u8, MAX_MESSAGE>,
    assembling: bool,
}

impl Decoder {
    /// Returns (gap detected, complete message). No partial message escapes.
    pub fn push(&mut self, packet: &[u8]) -> (bool, Option<heapless::Vec<u8, MAX_MESSAGE>>) {
        if packet.len() < 4 || packet.len() > MAX_PACKET || packet[2] & !3 != 0 {
            self.assembling = false;
            self.partial.clear();
            self.next = None;
            return (true, None);
        }
        let sequence = u16::from_le_bytes([packet[0], packet[1]]);
        let mut gap =
            self.next.is_some_and(|n| n != sequence) || (self.next.is_none() && packet[2] & 1 == 0);
        self.next = Some(sequence.wrapping_add(1));
        if gap {
            self.assembling = false;
            self.partial.clear();
        }
        if packet[2] & 1 != 0 {
            gap |= self.assembling;
            self.partial.clear();
            self.assembling = true;
        }
        if !self.assembling {
            return (gap, None);
        }
        if self.partial.extend_from_slice(&packet[3..]).is_err() {
            self.partial.clear();
            self.assembling = false;
            return (true, None);
        }
        if packet[2] & 2 != 0 {
            self.assembling = false;
            return (gap, Some(core::mem::take(&mut self.partial)));
        }
        (gap, None)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Waiting,
    Starting,
    Measuring,
    Fault,
}

impl Status {
    pub fn led_on(self, elapsed_ms: u64) -> bool {
        match self {
            Self::Waiting => elapsed_ms % 2000 < 1000,
            Self::Starting => elapsed_ms % 500 < 250,
            Self::Measuring => true,
            Self::Fault => elapsed_ms % 200 < 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmentation_wrap_and_recovery() {
        let mut d = Decoder::default();
        assert_eq!(d.push(&[255, 255, 1, b'A']), (false, None));
        let (gap, message) = d.push(&[0, 0, 2, b'B']);
        assert!(!gap);
        assert_eq!(message.unwrap().as_slice(), b"AB");
        d.push(&[1, 0, 1, b'C']);
        assert_eq!(d.push(&[3, 0, 2, b'D']), (true, None));
        let (gap, message) = d.push(&[4, 0, 3, b'E']);
        assert!(!gap);
        assert_eq!(message.unwrap().as_slice(), b"E");
    }

    #[test]
    fn cancelled_message_does_not_join_next_session() {
        let mut d = Decoder::default();
        d.push(&[0, 0, 1, b'A']);
        let (gap, message) = d.push(&[1, 0, 3, b'B']);
        assert!(gap);
        assert_eq!(message.unwrap().as_slice(), b"B");
        assert_eq!(d.push(&[2, 0, 0]), (true, None));
    }

    #[test]
    fn missing_first_fragment_is_reported_even_without_previous_sequence() {
        let mut d = Decoder::default();
        assert_eq!(d.push(&[15, 0, 2, b'B']), (true, None));
        let (gap, message) = d.push(&[16, 0, 3, b'C']);
        assert!(!gap);
        assert_eq!(message.unwrap().as_slice(), b"C");
    }

    #[test]
    fn oversize_message_is_discarded() {
        let mut d = Decoder::default();
        for n in 0..5_u8 {
            let mut p = [42; MAX_PACKET];
            p[..3].copy_from_slice(&[n, 0, u8::from(n == 0)]);
            assert_eq!(d.push(&p), (n == 4, None));
        }
        assert_eq!(d.push(&[5, 0, 2, 42]), (false, None));
    }

    #[test]
    fn led_patterns_are_distinct() {
        assert!(Status::Waiting.led_on(999));
        assert!(!Status::Waiting.led_on(1000));
        assert!(Status::Starting.led_on(249));
        assert!(!Status::Starting.led_on(250));
        assert!(Status::Fault.led_on(99));
        assert!(!Status::Fault.led_on(100));
        assert!((0..4000).all(|ms| Status::Measuring.led_on(ms)));
    }
}
