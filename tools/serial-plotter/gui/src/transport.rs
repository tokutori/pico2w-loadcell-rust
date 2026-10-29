//! Serial I/O boundary. A bounded queue transfers bytes to the GUI thread.
use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub enum Event {
    Opened,
    Bytes { at: Instant, bytes: Vec<u8> },
}

pub struct Session {
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<(), String>>,
}

impl Session {
    pub fn start(name: String, baud: u32) -> io::Result<Self> {
        let (sender, events) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("serial-reader".into())
            .spawn(move || read_port(&name, baud, &worker_stop, &sender))?;
        Ok(Self {
            events,
            stop,
            worker,
        })
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    pub fn finish(self) -> Result<(), String> {
        self.worker
            .join()
            .map_err(|_| "Serial reader thread panicked".to_owned())?
    }
}

fn send(sender: &SyncSender<Event>, event: Event) -> Result<(), String> {
    sender.try_send(event).map_err(|error| match error {
        mpsc::TrySendError::Full(_) => {
            "Receive queue full; disconnected to avoid silently losing data".into()
        }
        mpsc::TrySendError::Disconnected(_) => "Monitor closed".into(),
    })
}

fn read_port(
    name: &str,
    baud: u32,
    stop: &AtomicBool,
    sender: &SyncSender<Event>,
) -> Result<(), String> {
    let mut port = serialport::new(name, baud)
        .timeout(Duration::from_millis(100))
        .flow_control(serialport::FlowControl::None)
        .open()
        .map_err(|error| {
            format!("Cannot open {name}: {error}. Close other serial monitors, then reconnect.")
        })?;
    let result = (|| {
        port.write_data_terminal_ready(true)
            .map_err(|error| format!("Cannot enable DTR: {error}"))?;
        send(sender, Event::Opened)?;
        let mut bytes = [0_u8; 4096];
        while !stop.load(Ordering::Relaxed) {
            match port.read(&mut bytes) {
                Ok(0) => thread::sleep(Duration::from_millis(10)),
                Ok(count) => send(
                    sender,
                    Event::Bytes {
                        at: Instant::now(),
                        bytes: bytes[..count].to_vec(),
                    },
                )?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => {
                    return Err(format!(
                        "Serial read failed: {error}. Check USB and reconnect."
                    ));
                }
            }
        }
        Ok(())
    })();
    let _ = port.write_data_terminal_ready(false);
    result
}
