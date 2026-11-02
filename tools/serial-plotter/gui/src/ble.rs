//! BLE central. Discovery, setup and notification I/O stay off the UI thread.
use crate::transport::Event;
use btleplug::{
    api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter},
    platform::{Manager, Peripheral},
};
use futures_util::StreamExt;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

async fn stopped(stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        sleep(Duration::from_millis(20)).await;
    }
}

async fn send(sender: &SyncSender<Event>, mut event: Event) -> Result<()> {
    loop {
        match sender.try_send(event) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                event = returned;
                sleep(Duration::from_millis(2)).await;
            }
            Err(TrySendError::Disconnected(_)) => return Err("Monitor closed".into()),
        }
    }
}

type Result<T> = std::result::Result<T, String>;
async fn bounded<T>(future: impl std::future::Future<Output = btleplug::Result<T>>) -> Result<T> {
    timeout(Duration::from_secs(10), future)
        .await
        .map_err(|_| "BLE operation timed out".to_owned())?
        .map_err(|e| e.to_string())
}

pub async fn run(address: &str, stop: &AtomicBool, sender: &SyncSender<Event>) -> Result<()> {
    let setup = async {
        let manager = bounded(Manager::new()).await?;
        let adapters = bounded(manager.adapters()).await?;
        // Windows normally provides one OS Bluetooth radio. Report absence.
        adapters
            .into_iter()
            .next()
            .ok_or_else(|| "No Bluetooth adapter. Enable Bluetooth in Windows.".to_owned())
    };
    let adapter = tokio::select! { _ = stopped(stop) => return Ok(()), value = setup => value? };
    let mut selected: Option<Peripheral> = None;
    let work = async {
        send(sender, Event::Status("Scanning BLE for 5 s...".into())).await?;
        bounded(adapter.start_scan(ScanFilter {
            services: vec![Uuid::from_u128(loadcell_link::SERVICE)],
        }))
        .await?;
        sleep(Duration::from_secs(5)).await;
        let peripherals = bounded(adapter.peripherals()).await?;
        let mut matches = Vec::new();
        for peripheral in peripherals {
            if let Some(properties) = bounded(peripheral.properties()).await?
                && properties
                    .services
                    .contains(&Uuid::from_u128(loadcell_link::SERVICE))
                && (address.is_empty()
                    || peripheral
                        .address()
                        .to_string()
                        .eq_ignore_ascii_case(address))
            {
                matches.push(peripheral);
            }
        }
        bounded(adapter.stop_scan()).await?;
        if matches.len() != 1 {
            return Err(if matches.is_empty() {
                "No matching Pico Loadcells found. Check power, firmware and Bluetooth.".into()
            } else {
                format!(
                    "Multiple boards found. Enter BLE address: {}",
                    matches
                        .iter()
                        .map(|p| p.address().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        }
        let peripheral = matches.remove(0);
        selected = Some(peripheral.clone()); // Always clean up even if setup is cancelled.
        send(
            sender,
            Event::Status(format!("Connecting BLE {}...", peripheral.address())),
        )
        .await?;
        // Listen before connecting so a disconnect during setup is not lost.
        let mut events = bounded(adapter.events()).await?;
        bounded(peripheral.connect()).await?;
        bounded(peripheral.discover_services()).await?;
        let characteristic = peripheral
            .characteristics()
            .into_iter()
            .find(|c| {
                c.uuid == Uuid::from_u128(loadcell_link::TX)
                    && c.service_uuid == Uuid::from_u128(loadcell_link::SERVICE)
            })
            .ok_or_else(|| "Board does not provide Loadcells BLE protocol v1".to_owned())?;
        let mut notifications = bounded(peripheral.notifications()).await?;
        bounded(peripheral.subscribe(&characteristic)).await?;
        send(
            sender,
            Event::Status(format!(
                "BLE {} subscribed | USB takes priority | PC zero/calibration",
                peripheral.address()
            )),
        )
        .await?;
        let mut decoder = loadcell_link::Decoder::default();
        loop {
            tokio::select! {
                event = events.next() => match event {
                    Some(CentralEvent::DeviceDisconnected(id)) if id == peripheral.id() => return Err("BLE disconnected. Reconnect to start a new session.".into()),
                    None => return Err("Bluetooth adapter event stream closed".into()),
                    _ => {}
                },
                notification = notifications.next() => {
                    let n = notification.ok_or_else(|| "BLE notification stream closed".to_owned())?;
                    if n.uuid != characteristic.uuid { continue; }
                    let at = Instant::now();
                    let (gap, message) = decoder.push(&n.value);
                    if gap { send(sender, Event::Gap).await?; }
                    if let Some(message) = message {
                        send(sender, Event::Bytes { at, bytes: message.to_vec() }).await?;
                    }
                }
            }
        }
    };
    let result = tokio::select! { _ = stopped(stop) => Ok(()), result = work => result };
    // Each cleanup has a short deadline; Disconnect never waits indefinitely.
    let _ = timeout(Duration::from_secs(2), adapter.stop_scan()).await;
    if let Some(peripheral) = selected {
        let _ = timeout(Duration::from_secs(2), peripheral.disconnect()).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;

    fn packets(message: &str, mtu: usize, seq: &mut u16) -> Vec<Vec<u8>> {
        let chunks = message.as_bytes().chunks(mtu - 6);
        let count = chunks.len();
        chunks
            .enumerate()
            .map(|(i, bytes)| {
                let mut packet = seq.to_le_bytes().to_vec();
                *seq = seq.wrapping_add(1);
                packet.push(u8::from(i == 0) | (u8::from(i + 1 == count) << 1));
                packet.extend_from_slice(bytes);
                packet
            })
            .collect()
    }

    fn receive(
        decoder: &mut loadcell_link::Decoder,
        history: &mut History,
        at: f64,
        packet: &[u8],
    ) {
        let (gap, message) = decoder.push(packet);
        if gap {
            history.transport_gap();
        }
        if let Some(message) = message {
            history.ingest(at, &message);
        }
    }

    #[test]
    fn ble_framing_reaches_existing_parser_at_minimum_and_larger_mtu() {
        for mtu in [23, 128] {
            let mut decoder = loadcell_link::Decoder::default();
            let mut history = History::default();
            let mut seq = u16::MAX - 10;
            for sample in 1..=80 {
                for channel in 1..=4 {
                    let line = format!("LC{channel} raw=-155495\r\n");
                    for packet in packets(&line, mtu, &mut seq) {
                        receive(&mut decoder, &mut history, sample as f64 / 80.0, &packet);
                    }
                }
            }
            assert_eq!(history.errors, 0);
            assert_eq!(history.line_count, 320);
            assert_eq!(history.valid_samples_last_second(1.0), [80; 4]);
        }
    }

    #[test]
    fn lost_fragment_discards_whole_measurement_and_breaks_trace() {
        let mut decoder = loadcell_link::Decoder::default();
        let mut history = History::default();
        let mut seq = 0;
        let line = "LC1 raw=-155495 delta=-13982 kg~=0.181\r\n";
        let message = packets(line, 23, &mut seq);
        receive(&mut decoder, &mut history, 0.0, &message[0]);
        for packet in &message[2..] {
            receive(&mut decoder, &mut history, 0.1, packet);
        }
        assert_eq!(history.line_count, 0);
        assert_eq!(history.errors, 1);
        assert_eq!(history.discontinuities, [1; 4]);
        for packet in packets(line, 23, &mut seq) {
            receive(&mut decoder, &mut history, 0.2, &packet);
        }
        assert_eq!(history.line_count, 1);
        assert!(history.samples[0][0].break_before);
        assert_eq!(history.valid_samples_last_second(0.2), [1, 0, 0, 0]);
    }

    #[test]
    fn full_gui_queue_remains_cancellable() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
            sender.send(Event::Opened).unwrap();
            let stop = AtomicBool::new(false);
            let result = timeout(Duration::from_secs(1), async {
                tokio::select! {
                    _ = async { sleep(Duration::from_millis(5)).await; stop.store(true, Ordering::Relaxed); stopped(&stop).await; } => {},
                    _ = send(&sender, Event::Gap) => panic!("full queue must wait"),
                }
            }).await;
            assert!(result.is_ok());
        });
    }
}
