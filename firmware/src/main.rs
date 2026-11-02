#![no_std]
#![no_main]

mod hx711;
mod radio;

use core::fmt::Write;

use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, PIO0, USB};
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_rp::{dma, pio};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Sender, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config, Handler, UsbDevice};
use heapless::String;
use hx711::{Hx711, ReadError};
use loadcell_acquisition::session::Session;
use loadcell_acquisition::{Buffers, acquire};
use loadcell_link::Status;
use panic_halt as _;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

type UsbDriver = Driver<'static, USB>;
type Serial = Sender<'static, UsbDriver>;
type Device = UsbDevice<'static, UsbDriver>;

struct UsbLifecycle(&'static Session);

impl Handler for UsbLifecycle {
    fn reset(&mut self) {
        self.0.reset();
    }

    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            self.0.reset();
        }
    }

    fn configured(&mut self, configured: bool) {
        self.0.configured(configured);
    }

    fn suspended(&mut self, suspended: bool) {
        self.0.suspended(suspended);
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: Device) -> ! {
    usb.run().await
}

/// CDC-ACM endpoints carry packets, not arbitrary-length byte streams.
/// Send <=64 bytes per packet and end any exact multiple with ZLP.
async fn write_serial(serial: &mut Serial, message: &str) -> Result<(), EndpointError> {
    for chunk in message.as_bytes().chunks(64) {
        serial.write_packet(chunk).await?;
    }
    if message.len().is_multiple_of(64) {
        serial.write_packet(&[]).await?;
    }
    Ok(())
}

enum Sink<'a> {
    Usb(&'a mut Serial),
    Ble(&'a radio::Shared),
}
impl Sink<'_> {
    async fn write(&mut self, message: &str) -> Result<(), EndpointError> {
        match self {
            Self::Usb(serial) => write_serial(serial, message).await,
            Self::Ble(shared) => {
                shared.write(message).await;
                Ok(())
            }
        }
    }
}

fn usb_token(
    session: &Session,
    control: &ControlChanged<'_>,
) -> Option<loadcell_acquisition::session::Token> {
    session.current().filter(|_| control.dtr())
}

/// Reset the ADCs and stream validated raw words to the selected transport.
/// Cancellation only interrupts asynchronous waits; the GPIO clock transaction
/// in `Hx711::read` runs synchronously and always leaves PD_SCK low.
async fn run_measurements(
    serial: &mut Sink<'_>,
    sensors: &mut [Hx711<'_>; 4],
    status: &radio::Shared,
) -> Result<(), EndpointError> {
    status.status.set(Status::Starting);
    // Terminate any interrupted USB line; BLE frames writes atomically.
    serial.write("\r\nPico 2 W / HX711 x4\r\nSession started. Raw output; zero and calibration are PC-side.\r\n").await?;
    // Keep protocol initialization and settling even though tare is removed.
    // Each sensor resets to A/128 and waits 500 ms independently on first read.
    for sensor in sensors.iter_mut() {
        sensor.reset_before_next_read();
    }

    // Session-local queues and futures are all dropped on disconnect. No old
    // samples or worker tasks can leak into the next session.
    let buffers = Buffers::default();
    match select(
        acquire(sensors, &buffers),
        stream_measurements(serial, &buffers, status),
    )
    .await
    {
        Either::First(()) => unreachable!("acquisition runs until cancelled"),
        Either::Second(result) => result,
    }
}

/// The selected transport is the sole consumer; slow writes never suspend acquisition.
async fn stream_measurements(
    serial: &mut Sink<'_>,
    buffers: &Buffers<ReadError>,
    status: &radio::Shared,
) -> Result<(), EndpointError> {
    let mut receiver = buffers.receiver();
    let mut healthy = [false; 4];
    loop {
        let reading = receiver.receive().await;
        let i = reading.index;
        healthy[i] = reading.result.is_ok();
        status.status.set(if healthy.iter().all(|ok| *ok) {
            Status::Measuring
        } else {
            Status::Fault
        });
        let mut line: String<128> = String::new();
        if reading.dropped != 0 {
            write!(
                line,
                "Dropped LC{} samples={} (transport backlog)\r\n",
                i + 1,
                reading.dropped
            )
            .expect("line buffer capacity");
            serial.write(line.as_str()).await?;
            line.clear();
        }
        match reading.result {
            Ok(raw) => {
                writeln!(line, "LC{} raw={}\r", i + 1, raw).expect("line buffer capacity");
            }
            Err(error) => {
                let message = match error {
                    ReadError::Timeout => "timeout",
                    ReadError::Saturated => "saturated",
                    ReadError::Protocol => "protocol",
                };
                write!(line, "LC{} status={}\r\n", i + 1, message).expect("line buffer capacity");
            }
        }
        serial.write(line.as_str()).await?;
    }
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // The Akizuki FXMA108 board has an OE pull-up. OE=LOW enables it.
    // Creating this output HIGH keeps it disabled during initialization.
    let mut oe = Output::new(p.PIN_10, Level::High);
    let mut sensors = [
        Hx711::new(
            Input::new(p.PIN_2, Pull::None),
            Output::new(p.PIN_3, Level::Low),
        ),
        Hx711::new(
            Input::new(p.PIN_4, Pull::None),
            Output::new(p.PIN_5, Level::Low),
        ),
        Hx711::new(
            Input::new(p.PIN_6, Pull::None),
            Output::new(p.PIN_7, Level::Low),
        ),
        Hx711::new(
            Input::new(p.PIN_8, Pull::None),
            Output::new(p.PIN_9, Level::Low),
        ),
    ];

    let driver = Driver::new(p.USB, Irqs);
    let mut config = Config::new(0xc0de, 0xcafe); // experimental VID/PID only
    config.manufacturer = Some("Prototype");
    config.product = Some("Pico 2 W 4 Load Cells");
    config.serial_number = Some("HX711-4-01");
    config.max_power = 200;
    config.max_packet_size_0 = 64;

    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static STATE: StaticCell<State> = StaticCell::new();
    static SESSION: StaticCell<Session> = StaticCell::new();
    static LIFECYCLE: StaticCell<UsbLifecycle> = StaticCell::new();
    let session: &'static Session = SESSION.init(Session::default());

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, STATE.init(State::new()), 64);
    let (mut serial, _receiver, control) = class.split_with_control();
    builder.handler(LIFECYCLE.init(UsbLifecycle(session)));
    spawner.spawn(usb_task(builder.build()).expect("USB task allocation"));

    static RADIO: StaticCell<radio::Shared> = StaticCell::new();
    let radio = RADIO.init(radio::Shared::new());
    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = pio::Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        RM2_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );
    spawner.spawn(radio::start(spawner, pwr, spi, radio).expect("radio task allocation"));

    // Let the rails and the HX711s settle before enabling the translator.
    Timer::after_millis(100).await;
    oe.set_low();
    Timer::after_millis(500).await;

    loop {
        radio.select(false);
        radio.status.set(Status::Waiting);
        // One owner of all four sensors. Polling only connection state avoids
        // sharing USB Signal waiters and never blocks asynchronous acquisition.
        while usb_token(session, &control).is_none() && !radio.subscribed.get() {
            Timer::after_millis(10).await;
        }
        let result = if let Some(token) = usb_token(session, &control) {
            select(
                async {
                    while usb_token(session, &control) == Some(token) {
                        Timer::after_millis(10).await;
                    }
                },
                async {
                    serial.wait_connection().await;
                    run_measurements(&mut Sink::Usb(&mut serial), &mut sensors, radio).await
                },
            )
            .await
        } else {
            radio.select(true);
            let generation = radio.generation.get();
            select(
                async {
                    while radio.subscribed.get()
                        && radio.generation.get() == generation
                        && usb_token(session, &control).is_none()
                    {
                        Timer::after_millis(10).await;
                    }
                },
                run_measurements(&mut Sink::Ble(radio), &mut sensors, radio),
            )
            .await
        };
        if matches!(result, Either::Second(Err(EndpointError::BufferOverflow))) {
            panic!("USB packet exceeds endpoint capacity");
        }
        // Yield even if a disabled endpoint returned before its bus callback.
        Timer::after_millis(10).await;
    }
}
