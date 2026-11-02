//! Serial I/O boundary. A bounded queue transfers bytes to the GUI thread.
mod ble;
mod error;
pub use error::Error;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Usb(String),
    Ble(String),
}
impl std::fmt::Display for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usb(port) => write!(f, "Connected to {port} | DTR on | PC zero/calibration"),
            Self::Ble(address) => write!(
                f,
                "BLE {address} subscribed | USB takes priority | PC zero/calibration"
            ),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Progress {
    Scanning,
    Connecting(String),
}
impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scanning => f.write_str("Scanning BLE for 5 s..."),
            Self::Connecting(address) => write!(f, "Connecting BLE {address}..."),
        }
    }
}
pub enum Event {
    Ready(Link),
    Status(Progress),
    Gap,
    Bytes { at: Instant, bytes: Vec<u8> },
}

pub struct Session {
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<(), Error>>,
}

impl Session {
    #[cfg(test)]
    pub fn simulated() -> Self {
        let (sender, events) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            let _sender = sender;
            while !worker_stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        });
        Self {
            events,
            stop,
            worker,
        }
    }

    pub fn start_ble(address: String) -> io::Result<Self> {
        let (sender, events) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("ble-reader".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(Error::Runtime)?;
                rt.block_on(ble::run(&address, &worker_stop, &sender))
            })?;
        Ok(Self {
            events,
            stop,
            worker,
        })
    }
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

    pub fn finish(self) -> Result<(), Error> {
        self.worker.join().map_err(|_| Error::WorkerPanic)?
    }
}

fn send(sender: &SyncSender<Event>, mut event: Event, stop: &AtomicBool) -> Result<(), Error> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match sender.try_send(event) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(returned)) => {
                // Preserve this complete byte chunk and apply backpressure.
                // Poll the stop flag so Disconnect / closing the app never
                // waits indefinitely for the GUI to drain its own queue.
                event = returned;
                thread::sleep(Duration::from_millis(2));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return Err(Error::MonitorClosed),
        }
    }
}

/// Coalesce small USB reads for up to 10 ms (maximum 4096 bytes per event).
/// Queue capacity must not depend on the device's USB packet fragmentation.
fn read_stream(
    port: &mut impl Read,
    stop: &AtomicBool,
    sender: &SyncSender<Event>,
) -> Result<(), Error> {
    let mut bytes = [0_u8; 4096];
    let mut pending = Vec::with_capacity(4096);
    let mut last_received = Instant::now();
    let mut flushed_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let flush = match port.read(&mut bytes) {
            Ok(0) => {
                thread::sleep(Duration::from_millis(1));
                true
            }
            Ok(count) => {
                if pending.len() + count > bytes.len() {
                    send(
                        sender,
                        Event::Bytes {
                            at: last_received,
                            bytes: std::mem::take(&mut pending),
                        },
                        stop,
                    )?;
                    flushed_at = Instant::now();
                }
                pending.extend_from_slice(&bytes[..count]);
                last_received = Instant::now();
                pending.len() >= bytes.len() || flushed_at.elapsed() >= Duration::from_millis(10)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => false,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => true,
            Err(error) => {
                // Retain bytes already read before reporting a real I/O fault.
                if !pending.is_empty() {
                    send(
                        sender,
                        Event::Bytes {
                            at: last_received,
                            bytes: pending,
                        },
                        stop,
                    )?;
                }
                return Err(Error::Read(error));
            }
        };
        if flush && !pending.is_empty() {
            send(
                sender,
                Event::Bytes {
                    at: last_received,
                    bytes: std::mem::take(&mut pending),
                },
                stop,
            )?;
            flushed_at = Instant::now();
        }
    }
    Ok(())
}

fn read_port(
    name: &str,
    baud: u32,
    stop: &AtomicBool,
    sender: &SyncSender<Event>,
) -> Result<(), Error> {
    let mut port = serialport::new(name, baud)
        .timeout(Duration::from_millis(10))
        .flow_control(serialport::FlowControl::None)
        .open()
        .map_err(|source| Error::SerialOpen {
            port: name.into(),
            source,
        })?;
    let result = (|| {
        port.write_data_terminal_ready(true).map_err(Error::Dtr)?;
        send(sender, Event::Ready(Link::Usb(name.into())), stop)?;
        read_stream(&mut port, stop, sender)
    })();
    let _ = port.write_data_terminal_ready(false);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_sensor_burst_crosses_bounded_queue_and_parser_without_loss() {
        struct Fragmented {
            input: io::Cursor<Vec<u8>>,
            reads: usize,
        }
        impl Read for Fragmented {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let limit = [1, 7, 64, 513][self.reads % 4].min(out.len());
                self.reads += 1;
                match self.input.read(&mut out[..limit])? {
                    0 => Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "synthetic EOF",
                    )),
                    count => Ok(count),
                }
            }
        }
        // Data volume for 180 s at 80 SPS on all four channels, fed without
        // real-time sleeps. Exercise backpressure through a very small queue.
        let count = 80 * 180;
        let mut input = String::new();
        for sample in 0..count {
            for channel in [4, 2, 1, 3] {
                input.push_str(&format!(
                    "LC{channel} raw={sample} delta={sample} kg~={sample}\r\n"
                ));
            }
        }
        let (sender, receiver) = mpsc::sync_channel(2);
        let started = Instant::now();
        let worker = thread::spawn(move || {
            read_stream(
                &mut Fragmented {
                    input: io::Cursor::new(input.into_bytes()),
                    reads: 0,
                },
                &AtomicBool::new(false),
                &sender,
            )
        });
        let mut history = crate::history::History::default();
        for event in receiver {
            if let Event::Bytes { at, bytes } = event {
                assert!(bytes.len() <= 4096);
                history.ingest(at.duration_since(started).as_secs_f64(), &bytes);
            }
        }
        assert!(
            matches!(worker.join().unwrap(), Err(Error::Read(e)) if e.kind() == io::ErrorKind::UnexpectedEof)
        );
        assert_eq!(history.line_count, (count * 4) as u64);
        assert_eq!(history.errors, 0);
        assert!(history.pending().is_empty());
        for channel in serial_plotter_core::Channel::ALL {
            let samples = &history.samples[channel.index()];
            assert_eq!(samples.len(), crate::history::MAX_SAMPLES);
            for (i, sample) in samples.iter().enumerate() {
                assert_eq!(
                    sample.measurement.value(serial_plotter_core::Metric::Raw),
                    Some((count - crate::history::MAX_SAMPLES + i) as f64)
                );
            }
        }
    }

    #[test]
    fn tiny_usb_packets_are_batched_without_losing_bytes() {
        struct Burst {
            left: usize,
        }
        impl Read for Burst {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.left == 0 {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "end of fixture"));
                }
                let line = b"LC1 raw=-1 delta=213881 kg~=-2.767\r\n";
                bytes[..line.len()].copy_from_slice(line);
                self.left -= 1;
                Ok(line.len())
            }
        }
        // Previously 500 small reads exhausted the 64-event queue even though
        // the total byte count was tiny compared with its intended capacity.
        let (sender, receiver) = mpsc::sync_channel(64);
        let stop = AtomicBool::new(false);
        assert!(
            matches!(read_stream(&mut Burst { left: 500 }, &stop, &sender), Err(Error::Read(e)) if e.kind() == io::ErrorKind::BrokenPipe)
        );
        let mut events = 0;
        let mut received = Vec::new();
        for event in receiver.try_iter() {
            if let Event::Bytes { bytes, .. } = event {
                assert!(bytes.len() <= 4096);
                events += 1;
                received.extend(bytes);
            }
        }
        assert!(events < 64);
        assert_eq!(
            received,
            b"LC1 raw=-1 delta=213881 kg~=-2.767\r\n".repeat(500)
        );
    }

    #[test]
    fn full_queue_waits_for_capacity_instead_of_disconnecting() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(Event::Gap).unwrap();
        let worker = thread::spawn(move || {
            send(
                &sender,
                Event::Bytes {
                    at: Instant::now(),
                    bytes: b"retained".to_vec(),
                },
                &AtomicBool::new(false),
            )
        });
        thread::sleep(Duration::from_millis(10));
        assert!(!worker.is_finished());
        assert!(matches!(receiver.recv().unwrap(), Event::Gap));
        assert!(worker.join().unwrap().is_ok());
        match receiver.recv().unwrap() {
            Event::Bytes { bytes, .. } => assert_eq!(bytes, b"retained"),
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn stop_cancels_a_send_waiting_on_a_full_queue() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender.send(Event::Gap).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || send(&sender, Event::Gap, &worker_stop));
        stop.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap().is_ok());
    }
}
