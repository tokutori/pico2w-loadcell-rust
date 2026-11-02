//! Exercise the same acquisition futures used by firmware, with controlled
//! readiness instead of GPIO/real time. No duplicate scheduler model.
use core::{
    future::{Future, poll_fn},
    pin::Pin,
    task::{Context, Poll, Waker},
};
use loadcell_acquisition::{BUFFER_CAPACITY, Buffers, Reading, Source, acquire, tare};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    Timeout,
}

#[derive(Default)]
struct Control {
    ready: RefCell<VecDeque<Result<i32, Fault>>>,
    waker: RefCell<Option<Waker>>,
    started: Cell<usize>,
    cancelled: Cell<usize>,
}

impl Control {
    fn send(&self, result: Result<i32, Fault>) {
        self.ready.borrow_mut().push_back(result);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

struct Mock(Rc<Control>);
struct PendingRead {
    control: Rc<Control>,
    completed: bool,
}
impl Drop for PendingRead {
    fn drop(&mut self) {
        if !self.completed {
            self.control.cancelled.set(self.control.cancelled.get() + 1);
        }
    }
}

impl Source for Mock {
    type Error = Fault;
    async fn read(&mut self) -> Result<i32, Fault> {
        self.0.started.set(self.0.started.get() + 1);
        let mut pending = PendingRead {
            control: self.0.clone(),
            completed: false,
        };
        let value = poll_fn(|cx| {
            if let Some(result) = self.0.ready.borrow_mut().pop_front() {
                Poll::Ready(result)
            } else {
                *self.0.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
        pending.completed = true;
        value
    }
}

fn sources() -> ([Mock; 4], [Rc<Control>; 4]) {
    let controls: [Rc<Control>; 4] = std::array::from_fn(|_| Rc::default());
    (std::array::from_fn(|i| Mock(controls[i].clone())), controls)
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn receive(receiver: &mut loadcell_acquisition::Receiver<'_, Fault>) -> Reading<Fault> {
    let mut future = std::pin::pin!(receiver.receive());
    match poll(future.as_mut()) {
        Poll::Ready(reading) => reading,
        Poll::Pending => panic!("ready sensor was blocked"),
    }
}

#[test]
fn fourth_sensor_streams_while_first_three_are_waiting() {
    let (mut sources, controls) = sources();
    let buffers = Buffers::default();
    let mut receiver = buffers.receiver();
    let mut acquisition = std::pin::pin!(acquire(&mut sources, &buffers));
    assert!(poll(acquisition.as_mut()).is_pending());
    assert!(controls.iter().all(|control| control.started.get() == 1));
    for raw in [100, 101, 102] {
        controls[3].send(Ok(raw));
        assert!(poll(acquisition.as_mut()).is_pending());
        assert_eq!(
            receive(&mut receiver),
            Reading {
                index: 3,
                result: Ok(raw),
                dropped: 0
            }
        );
    }
    controls[0].send(Err(Fault::Timeout));
    controls[2].send(Ok(-200));
    assert!(poll(acquisition.as_mut()).is_pending());
    assert_eq!(receive(&mut receiver).result, Err(Fault::Timeout));
    assert_eq!(
        receive(&mut receiver),
        Reading {
            index: 2,
            result: Ok(-200),
            dropped: 0
        }
    );
}

#[test]
fn tare_attempts_progress_independently_and_reject_partial_success() {
    let (mut sources, controls) = sources();
    let mut operation = std::pin::pin!(tare(&mut sources, 12));
    assert!(poll(operation.as_mut()).is_pending());
    assert!(controls.iter().all(|control| control.started.get() == 1));
    for attempt in 0..12 {
        controls[0].send(Ok(-100));
        controls[2].send(Ok(300));
        controls[3].send(if attempt == 0 {
            Ok(400)
        } else {
            Err(Fault::Timeout)
        });
        assert!(poll(operation.as_mut()).is_pending());
    }
    assert!(controls[0].started.get() == 12 && controls[1].started.get() == 1);
    for _ in 0..12 {
        controls[1].send(Err(Fault::Timeout));
        assert!(poll(operation.as_mut()).is_pending());
    }
    assert_eq!(
        poll(operation.as_mut()),
        Poll::Ready([Some(-100), None, Some(300), None])
    );
}

#[test]
fn tare_requires_all_samples_and_computes_mean_including_signed_and_zero_values() {
    let (mut sources, controls) = sources();
    for (index, samples) in [
        [Ok(-3), Ok(0), Ok(3)],
        [Err(Fault::Timeout), Ok(100), Ok(100)],
        [Ok(100), Ok(100), Err(Fault::Timeout)],
        [Ok(-300), Ok(-600), Ok(-900)],
    ]
    .into_iter()
    .enumerate()
    {
        for sample in samples {
            controls[index].send(sample);
        }
    }
    let mut operation = std::pin::pin!(tare(&mut sources, 3));
    for _ in 0..3 {
        assert!(poll(operation.as_mut()).is_pending());
    }
    assert_eq!(
        poll(operation.as_mut()),
        Poll::Ready([Some(0), None, None, Some(-600)])
    );
}

#[test]
fn empty_tare_does_not_read_or_create_a_zero_offset() {
    let (mut sources, controls) = sources();
    assert_eq!(
        poll(std::pin::pin!(tare(&mut sources, 0)).as_mut()),
        Poll::Ready([None; 4])
    );
    assert!(controls.iter().all(|control| control.started.get() == 0));
}

#[test]
fn stalled_usb_does_not_block_acquisition_and_overflow_is_counted_per_sensor() {
    let (mut sources, controls) = sources();
    let buffers = Buffers::default();
    let mut receiver = buffers.receiver();
    let mut acquisition = std::pin::pin!(acquire(&mut sources, &buffers));
    for raw in 0..40 {
        for control in &controls {
            control.send(Ok(raw));
        }
        assert!(poll(acquisition.as_mut()).is_pending());
    }
    // All sources kept reading despite the consumer never running.
    assert!(controls.iter().all(|control| control.started.get() == 40));
    for index in 0..4 {
        assert_eq!(
            receive(&mut receiver),
            Reading {
                index,
                result: Ok(40 - BUFFER_CAPACITY as i32),
                dropped: 40 - BUFFER_CAPACITY as u32,
            }
        );
    }
    // A repeatedly-ready first queue does not starve the others.
    for index in 0..4 {
        controls[0].send(Ok(500));
        assert!(poll(acquisition.as_mut()).is_pending());
        assert_eq!(receive(&mut receiver).index, index);
    }
}

#[test]
fn disconnect_cancels_all_pending_reads_and_new_session_has_no_old_samples() {
    let (mut sources, controls) = sources();
    {
        let buffers = Buffers::default();
        let mut acquisition = std::pin::pin!(acquire(&mut sources, &buffers));
        assert!(poll(acquisition.as_mut()).is_pending());
        controls[0].send(Ok(123));
        assert!(poll(acquisition.as_mut()).is_pending());
        assert!(poll(acquisition.as_mut()).is_pending());
    }
    assert!(controls.iter().all(|control| control.cancelled.get() == 1));
    let buffers = Buffers::default();
    let mut receiver = buffers.receiver();
    let mut acquisition = std::pin::pin!(acquire(&mut sources, &buffers));
    assert!(poll(acquisition.as_mut()).is_pending());
    assert!(poll(std::pin::pin!(receiver.receive()).as_mut()).is_pending());
    controls[2].send(Ok(999));
    assert!(poll(acquisition.as_mut()).is_pending());
    assert_eq!(
        receive(&mut receiver),
        Reading {
            index: 2,
            result: Ok(999),
            dropped: 0
        }
    );
}
