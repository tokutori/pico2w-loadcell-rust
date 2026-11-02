//! Four independent acquisition loops and bounded output buffering.
//! No GPIO, clock, USB, heap allocation, or executor-specific task spawning.
//! All futures and buffers must be used on the same executor thread.
#![no_std]

pub mod hx711;
pub mod session;

use core::{cell::Cell, future::Future};
use embassy_futures::{
    join::join4,
    select::{Either4, select4},
    yield_now,
};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Channel, TrySendError},
};

pub const SENSOR_COUNT: usize = 4;
pub const BUFFER_CAPACITY: usize = 16;

/// Each read must have its own finite timeout. Cancellation must leave the
/// physical interface idle so that reconnecting can start a fresh session.
pub trait Source {
    type Error;
    fn read(&mut self) -> impl Future<Output = Result<i32, Self::Error>>;
}

/// Mean only when all `attempts` readings succeed, independently per source.
/// A failed/empty tare must not silently calibrate from a partial sample set.
/// Completion waits for all four tare operations, not their summed durations.
pub async fn tare<S: Source>(
    sources: &mut [S; SENSOR_COUNT],
    attempts: u16,
) -> [Option<i32>; SENSOR_COUNT] {
    async fn one<S: Source>(source: &mut S, attempts: u16) -> Option<i32> {
        let mut sum = 0_i64;
        let mut count = 0_u32;
        for _ in 0..attempts {
            if let Ok(value) = source.read().await {
                sum += i64::from(value);
                count += 1;
            }
            yield_now().await;
        }
        (count != 0 && count == u32::from(attempts)).then(|| (sum / i64::from(count)) as i32)
    }
    let [a, b, c, d] = sources;
    let (a, b, c, d) = join4(
        one(a, attempts),
        one(b, attempts),
        one(c, attempts),
        one(d, attempts),
    )
    .await;
    [a, b, c, d]
}

/// One bounded queue per sensor prevents a noisy input from filling the other
/// sensors' queues. Overflow drops the oldest entry and counts every loss.
pub struct Buffers<E> {
    queues: [Channel<NoopRawMutex, Result<i32, E>, BUFFER_CAPACITY>; SENSOR_COUNT],
    dropped: [Cell<u32>; SENSOR_COUNT],
}

impl<E> Default for Buffers<E> {
    fn default() -> Self {
        Self {
            queues: core::array::from_fn(|_| Channel::new()),
            dropped: core::array::from_fn(|_| Cell::new(0)),
        }
    }
}

impl<E> Buffers<E> {
    fn push(&self, index: usize, result: Result<i32, E>) {
        let queue = &self.queues[index];
        if let Err(TrySendError::Full(result)) = queue.try_send(result) {
            // No await: single-threaded producer/consumer cannot interleave here.
            assert!(queue.try_receive().is_ok());
            self.dropped[index].set(self.dropped[index].get().saturating_add(1));
            assert!(queue.try_send(result).is_ok());
        }
    }

    pub fn receiver(&self) -> Receiver<'_, E> {
        Receiver {
            buffers: self,
            next: 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reading<E> {
    /// Zero-based physical sensor index (0..4).
    pub index: usize,
    pub result: Result<i32, E>,
    /// Entries discarded since the previous delivery for this sensor.
    pub dropped: u32,
}

/// Fair round-robin draining of available queues. An empty queue never blocks
/// another sensor. If all queues are empty, sleep until any sensor publishes.
pub struct Receiver<'a, E> {
    buffers: &'a Buffers<E>,
    next: usize,
}

impl<E> Receiver<'_, E> {
    fn delivered(&mut self, index: usize, result: Result<i32, E>) -> Reading<E> {
        self.next = (index + 1) % SENSOR_COUNT;
        Reading {
            index,
            result,
            dropped: self.buffers.dropped[index].replace(0),
        }
    }

    pub async fn receive(&mut self) -> Reading<E> {
        for offset in 0..SENSOR_COUNT {
            let index = (self.next + offset) % SENSOR_COUNT;
            if let Ok(result) = self.buffers.queues[index].try_receive() {
                return self.delivered(index, result);
            }
        }
        let q = &self.buffers.queues;
        let (index, result) = match select4(
            q[0].receive(),
            q[1].receive(),
            q[2].receive(),
            q[3].receive(),
        )
        .await
        {
            Either4::First(result) => (0, result),
            Either4::Second(result) => (1, result),
            Either4::Third(result) => (2, result),
            Either4::Fourth(result) => (3, result),
        };
        self.delivered(index, result)
    }
}

/// Run until cancelled. In particular, do not join four individual reads into
/// batches: each loop starts its next read without waiting for another sensor.
pub async fn acquire<S: Source>(sources: &mut [S; SENSOR_COUNT], buffers: &Buffers<S::Error>) {
    async fn one<S: Source>(source: &mut S, index: usize, buffers: &Buffers<S::Error>) {
        loop {
            buffers.push(index, source.read().await);
            // Even a continuously-ready broken input cannot monopolize a poll.
            yield_now().await;
        }
    }
    let [a, b, c, d] = sources;
    join4(
        one(a, 0, buffers),
        one(b, 1, buffers),
        one(c, 2, buffers),
        one(d, 3, buffers),
    )
    .await;
}
