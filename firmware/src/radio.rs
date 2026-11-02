//! CYW43439 owns both BLE and the Pico 2 W onboard LED (WL_GPIO0).
// trouble-host 0.6's GATT macro borrows the generated Vec default value.
#![allow(clippy::needless_borrows_for_generic_args)]
use core::cell::Cell;
use cyw43::aligned_bytes;
use cyw43_pio::PioSpi;
use embassy_executor::Spawner;
use embassy_futures::{join::join, select::select};
use embassy_rp::{gpio::Output, peripherals::PIO0};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Instant, Timer};
use heapless::{String, Vec};
use loadcell_link::{MAX_MESSAGE, MAX_PACKET, Status};
use static_cell::StaticCell;
use trouble_host::prelude::*;

pub struct Shared {
    pub subscribed: Cell<bool>,
    pub generation: Cell<u64>,
    pub selected: Cell<bool>,
    pub status: Cell<Status>,
    pub failed: Cell<bool>,
    queue: Channel<NoopRawMutex, (u64, String<MAX_MESSAGE>), 8>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            subscribed: Cell::new(false),
            generation: Cell::new(0),
            selected: Cell::new(false),
            status: Cell::new(Status::Waiting),
            failed: Cell::new(false),
            queue: Channel::new(),
        }
    }

    pub fn select(&self, selected: bool) {
        self.selected.set(selected);
        self.generation.set(self.generation.get().wrapping_add(1));
        self.queue.clear();
    }

    fn subscribe(&self, value: bool) {
        if self.subscribed.replace(value) != value {
            self.select(false);
        }
    }

    pub async fn write(&self, message: &str) {
        let text = String::try_from(message).expect("BLE message capacity");
        self.queue.send((self.generation.get(), text)).await;
    }
}

type Spi = PioSpi<'static, PIO0, 0>;
#[embassy_executor::task]
async fn runner_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, Spi>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
pub async fn start(spawner: Spawner, pwr: Output<'static>, spi: Spi, shared: &'static Shared) {
    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let (_, bt, mut control, runner) = cyw43::new_with_bluetooth(
        STATE.init(cyw43::State::new()),
        pwr,
        spi,
        aligned_bytes!("../cyw43-firmware/43439A0.bin"),
        aligned_bytes!("../cyw43-firmware/43439A0_btfw.bin"),
        aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin"),
    )
    .await;
    spawner.spawn(runner_task(runner).expect("CYW43 task allocation"));
    control
        .init(aligned_bytes!("../cyw43-firmware/43439A0_clm.bin"))
        .await;
    // Derive a static random BLE address from the board's unique factory MAC.
    // bt-hci addresses are little-endian; the most significant byte is last.
    let mut address = control.address().await;
    address.reverse();
    address[5] |= 0xc0;
    let controller: ExternalController<_, 10> = ExternalController::new(bt);
    join(
        ble(controller, Address::random(address), shared),
        led(control, shared),
    )
    .await;
}

async fn led(mut control: cyw43::Control<'_>, shared: &Shared) {
    let mut previous = None;
    loop {
        let status = if shared.failed.get() {
            Status::Fault
        } else {
            shared.status.get()
        };
        let on = status.led_on(Instant::now().as_millis());
        if previous != Some(on) {
            control.gpio_set(0, on).await;
            previous = Some(on);
        }
        Timer::after_millis(25).await;
    }
}

#[gatt_server]
struct Server {
    telemetry: Telemetry,
}

#[gatt_service(uuid = "854b6df0-3908-4ae5-8862-89c122e17401")]
struct Telemetry {
    #[characteristic(uuid = "854b6df1-3908-4ae5-8862-89c122e17401", notify)]
    tx: Vec<u8, MAX_PACKET>,
}

async fn ble<C: Controller>(controller: C, address: Address, shared: &Shared) {
    let mut resources: HostResources<DefaultPacketPool, 1, 2> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut peripheral,
        mut runner,
        ..
    } = stack.build();
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "Pico Loadcells",
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    }))
    .expect("GATT table capacity");
    let mut seq = 0_u16;
    select(runner.run(), async {
        loop {
            let mut adv = [0; 31];
            let len = AdStructure::encode_slice(
                &[
                    AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                    AdStructure::ServiceUuids128(&[loadcell_link::SERVICE.to_le_bytes()]),
                ],
                &mut adv,
            )
            .expect("advertisement capacity");
            let mut scan = [0; 31];
            let scan_len = AdStructure::encode_slice(
                &[AdStructure::CompleteLocalName(b"Pico Loadcells")],
                &mut scan,
            )
            .expect("scan response capacity");
            let result = peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv[..len],
                        scan_data: &scan[..scan_len],
                    },
                )
                .await;
            if let Ok(advertiser) = result {
                if let Ok(conn) = advertiser.accept().await
                    && let Ok(conn) = conn.with_attribute_server(&server)
                {
                    shared.failed.set(false);
                    select(
                        events(&server, &conn, shared, &stack),
                        notify(&server, &conn, shared, &mut seq),
                    )
                    .await;
                }
            } else {
                shared.failed.set(true);
            }
            shared.subscribe(false);
            Timer::after_millis(100).await;
        }
    })
    .await;
    // Preserve USB operation and LED indication if the BLE host stops.
    shared.subscribe(false);
    shared.failed.set(true);
}

async fn events<C: Controller>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    shared: &Shared,
    stack: &Stack<'_, C, DefaultPacketPool>,
) {
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { .. } => return,
            GattConnectionEvent::Gatt { event } => {
                let subscription = match &event {
                    GattEvent::Write(write)
                        if Some(write.handle()) == server.telemetry.tx.cccd_handle =>
                    {
                        let data = write.data();
                        (data.len() == 2).then(|| data[0] & 1 != 0)
                    }
                    _ => None,
                };
                if let Ok(reply) = event.accept() {
                    reply.send().await;
                    if let Some(enabled) = subscription {
                        shared.subscribe(enabled);
                    }
                }
            }
            GattConnectionEvent::RequestConnectionParams(request) => {
                let _ = request.accept(None, stack).await;
            }
            _ => {}
        }
    }
}

async fn notify(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    shared: &Shared,
    seq: &mut u16,
) {
    loop {
        let (generation, message) = shared.queue.receive().await;
        let size = (conn.raw().att_mtu() as usize - 3).min(MAX_PACKET) - 3;
        let chunks = message.as_bytes().chunks(size);
        let count = chunks.len();
        for (i, chunk) in chunks.enumerate() {
            if !shared.selected.get()
                || !shared.subscribed.get()
                || shared.generation.get() != generation
            {
                break;
            }
            let mut packet = Vec::<u8, MAX_PACKET>::new();
            packet.extend_from_slice(&seq.to_le_bytes()).unwrap();
            packet
                .push(u8::from(i == 0) | (u8::from(i + 1 == count) << 1))
                .unwrap();
            packet.extend_from_slice(chunk).unwrap();
            *seq = seq.wrapping_add(1);
            if server.telemetry.tx.notify(conn, &packet).await.is_err() {
                return;
            }
        }
    }
}
