use std::{fmt, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Manager,
    Adapters,
    Scan,
    Devices,
    Properties,
    StopScan,
    Events,
    Connect,
    DiscoverServices,
    Notifications,
    Subscribe,
}
impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manager => "create manager",
            Self::Adapters => "list adapters",
            Self::Scan => "start scan",
            Self::Devices => "list devices",
            Self::Properties => "read device properties",
            Self::StopScan => "stop scan",
            Self::Events => "open adapter events",
            Self::Connect => "connect",
            Self::DiscoverServices => "discover services",
            Self::Notifications => "open notifications",
            Self::Subscribe => "subscribe",
        })
    }
}

#[derive(Debug)]
pub enum Error {
    Runtime(io::Error),
    WorkerPanic,
    MonitorClosed,
    SerialOpen {
        port: String,
        source: serialport::Error,
    },
    Dtr(serialport::Error),
    Read(io::Error),
    Ble {
        operation: Operation,
        source: btleplug::Error,
    },
    Timeout(Operation),
    NoAdapter,
    NoBoard,
    MultipleBoards(Vec<String>),
    MissingService,
    Disconnected,
    AdapterClosed,
    NotificationsClosed,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(e) => write!(f, "Cannot start BLE runtime: {e}"),
            Self::WorkerPanic => f.write_str("Transport worker panicked"),
            Self::MonitorClosed => f.write_str("Monitor closed"),
            Self::SerialOpen { port, source } => write!(
                f,
                "Cannot open {port}: {source}. Close other serial monitors, then reconnect."
            ),
            Self::Dtr(e) => write!(f, "Cannot enable DTR: {e}"),
            Self::Read(e) => write!(f, "Serial read failed: {e}. Check USB and reconnect."),
            Self::Ble { operation, source } => write!(f, "BLE {operation} failed: {source}"),
            Self::Timeout(op) => write!(f, "BLE {op} timed out"),
            Self::NoAdapter => f.write_str("No Bluetooth adapter. Enable Bluetooth in Windows."),
            Self::NoBoard => f.write_str(
                "No matching Pico Loadcells found. Check power, firmware and Bluetooth.",
            ),
            Self::MultipleBoards(addresses) => write!(
                f,
                "Multiple boards found. Enter BLE address: {}",
                addresses.join(", ")
            ),
            Self::MissingService => f.write_str("Board does not provide Loadcells BLE protocol v1"),
            Self::Disconnected => {
                f.write_str("BLE disconnected. Reconnect to start a new session.")
            }
            Self::AdapterClosed => f.write_str("Bluetooth adapter event stream closed"),
            Self::NotificationsClosed => f.write_str("BLE notification stream closed"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(e) | Self::Read(e) => Some(e),
            Self::SerialOpen { source, .. } | Self::Dtr(source) => Some(source),
            Self::Ble { source, .. } => Some(source),
            _ => None,
        }
    }
}
