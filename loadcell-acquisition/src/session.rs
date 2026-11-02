//! Acquisition lifetime across USB configuration, reset and suspend events.
//! Like the acquisition queues, this state has one waiter on one executor.
use core::cell::Cell;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};

#[derive(Default)]
pub struct Session {
    configured: Cell<bool>,
    suspended: Cell<bool>,
    generation: Cell<u64>,
    changed: Signal<NoopRawMutex, ()>,
}

/// Identifies one uninterrupted configured, awake USB session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token(u64);

impl Session {
    /// Snapshot used by a transport arbiter on the same executor.
    pub fn current(&self) -> Option<Token> {
        (self.configured.get() && !self.suspended.get()).then(|| Token(self.generation.get()))
    }
    fn notify(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.changed.signal(());
    }

    pub fn reset(&self) {
        self.configured.set(false);
        self.suspended.set(false);
        self.notify();
    }

    pub fn configured(&self, configured: bool) {
        self.configured.set(configured);
        // Even SET_CONFIGURATION(1) again reinitializes endpoints.
        self.notify();
    }

    pub fn suspended(&self, suspended: bool) {
        if self.suspended.replace(suspended) != suspended {
            self.notify();
        }
    }

    pub async fn wait_start(&self) -> Token {
        while !self.configured.get() || self.suspended.get() {
            self.changed.wait().await;
        }
        Token(self.generation.get())
    }

    pub async fn wait_end(&self, token: Token) {
        // A generation detects reset/resume even if both events happened
        // before this future was polled. A coalesced boolean signal alone
        // could incorrectly continue streaming with an old calibration.
        while self.generation.get() == token.0 {
            self.changed.wait().await;
        }
    }
}
