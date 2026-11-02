use core::{
    future::{Future, pending},
    pin::Pin,
    task::{Context, Poll, Waker},
};
use embassy_futures::select::{Either, select};
use loadcell_acquisition::{Buffers, Source, acquire, session::Session};
use std::{cell::Cell, rc::Rc};

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn start(session: &Session) -> loadcell_acquisition::session::Token {
    match poll(std::pin::pin!(session.wait_start()).as_mut()) {
        Poll::Ready(token) => token,
        Poll::Pending => panic!("USB should be available"),
    }
}

#[test]
fn arbiter_snapshot_detects_suspension_and_coalesced_reconfiguration() {
    let session = Session::default();
    assert_eq!(session.current(), None);
    session.configured(true);
    let old = session.current().unwrap();
    session.suspended(true);
    assert_eq!(session.current(), None);
    session.suspended(false);
    assert_ne!(session.current(), Some(old));
    let resumed = session.current();
    session.reset();
    session.configured(true);
    assert_ne!(session.current(), resumed);
    session.configured(false);
    assert_eq!(session.current(), None);
}

#[test]
fn enumeration_required_and_unconfiguration_ends_session() {
    let session = Session::default();
    assert!(poll(std::pin::pin!(session.wait_start()).as_mut()).is_pending());
    session.configured(true);
    let token = start(&session);
    assert!(poll(std::pin::pin!(session.wait_end(token)).as_mut()).is_pending());
    session.configured(false);
    assert!(poll(std::pin::pin!(session.wait_end(token)).as_mut()).is_ready());
    assert!(poll(std::pin::pin!(session.wait_start()).as_mut()).is_pending());
    session.configured(true);
    assert_ne!(start(&session), token);
}

#[test]
fn suspension_requires_resume_and_reset_requires_configuration() {
    let session = Session::default();
    session.configured(true);
    let token = start(&session);
    session.suspended(true);
    assert!(poll(std::pin::pin!(session.wait_end(token)).as_mut()).is_ready());
    assert!(poll(std::pin::pin!(session.wait_start()).as_mut()).is_pending());
    session.suspended(false);
    let resumed = start(&session);
    assert_ne!(resumed, token);
    session.suspended(false); // repeated awake notification is not a new session
    assert!(poll(std::pin::pin!(session.wait_end(resumed)).as_mut()).is_pending());
    session.reset();
    assert!(poll(std::pin::pin!(session.wait_end(resumed)).as_mut()).is_ready());
    session.suspended(false);
    assert!(poll(std::pin::pin!(session.wait_start()).as_mut()).is_pending());
}

#[test]
fn rapid_bus_changes_cannot_hide_a_session_boundary() {
    let session = Session::default();
    session.configured(true);
    for event in 0..3 {
        let old = start(&session);
        match event {
            0 => {
                session.reset();
                session.configured(true);
            }
            1 => {
                session.suspended(true);
                session.suspended(false);
            }
            _ => session.configured(true), // endpoints reinitialized
        }
        // Both changes occurred before the waiter was polled again.
        assert!(poll(std::pin::pin!(session.wait_end(old)).as_mut()).is_ready());
        assert_ne!(start(&session), old);
    }
}

struct OnDrop(Rc<Cell<usize>>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

struct WaitingSensor(Rc<Cell<usize>>);
impl Source for WaitingSensor {
    type Error = ();
    async fn read(&mut self) -> Result<i32, ()> {
        let _cancelled = OnDrop(self.0.clone());
        pending().await
    }
}

#[test]
fn bus_event_cancels_all_reads_and_blocked_usb_even_without_dtr_change() {
    for event in 0..3 {
        let session = Session::default();
        session.configured(true);
        let token = start(&session);
        let read_drops = Rc::new(Cell::new(0));
        let write_drops = Rc::new(Cell::new(0));
        let mut sensors = core::array::from_fn(|_| WaitingSensor(read_drops.clone()));
        let buffers = Buffers::default();
        {
            let mut running = std::pin::pin!(select(session.wait_end(token), async {
                select(acquire(&mut sensors, &buffers), async {
                    let _cancelled = OnDrop(write_drops.clone());
                    pending::<()>().await;
                })
                .await
            }));
            assert!(poll(running.as_mut()).is_pending());
            match event {
                0 => session.reset(),
                1 => session.configured(false),
                _ => session.suspended(true),
            }
            assert!(matches!(
                poll(running.as_mut()),
                Poll::Ready(Either::First(()))
            ));
        }
        assert_eq!(read_drops.get(), 4);
        assert_eq!(write_drops.get(), 1);
    }
}
