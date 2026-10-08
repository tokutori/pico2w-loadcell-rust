#![no_std]
#![no_main]

mod hx711;

use core::fmt::Write;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Sender, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config, UsbDevice};
use heapless::String;
use hx711::{Hx711, ReadError};
use loadcell_core::{Calibration, NOMINAL_COUNTS_PER_KG};
use panic_halt as _;
use static_cell::StaticCell;

/// Initial, **uncalibrated** sensitivities. Replace each value after measuring
/// a known mass on each of the four sensors (signed counts / kg).
const COUNTS_PER_KG: [i32; 4] = [NOMINAL_COUNTS_PER_KG; 4];
const TARE_SAMPLES: usize = 12;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

type UsbDriver = Driver<'static, USB>;
type Serial = Sender<'static, UsbDriver>;
type Device = UsbDevice<'static, UsbDriver>;

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

/// Take the mean of successful samples from each sensor. Sensors which do
/// not respond remain `None` and do not suppress measurements from others.
async fn tare(sensors: &mut [Hx711<'_>; 4]) -> [Option<i32>; 4] {
    let mut sums = [0_i64; 4];
    let mut counts = [0_u32; 4];
    for _ in 0..TARE_SAMPLES {
        for (i, sensor) in sensors.iter_mut().enumerate() {
            if let Ok(value) = sensor.read().await {
                sums[i] += i64::from(value);
                counts[i] += 1;
            }
        }
    }
    core::array::from_fn(|i| match counts[i] {
        0 => None,
        n => Some((sums[i] / i64::from(n)) as i32),
    })
}

/// Closing the terminal or resetting USB ends the current measurement session.
/// Racing this against measurements also cancels a blocked endpoint write.
async fn wait_disconnect(control: &ControlChanged<'_>) {
    while control.dtr() {
        control.control_changed().await;
    }
}

/// Tare once after the terminal opens, then stream until USB reports an error.
/// Cancellation only interrupts asynchronous waits; the GPIO clock transaction
/// in `Hx711::read` runs synchronously and always leaves PD_SCK low.
async fn run_measurements(
    serial: &mut Serial,
    sensors: &mut [Hx711<'_>; 4],
) -> Result<(), EndpointError> {
    write_serial(
        serial,
        "Pico 2 W / HX711 x4\r\nRemove all loads; tare starts in 3 seconds.\r\n",
    )
    .await?;
    Timer::after_secs(3).await;

    let offsets = tare(sensors).await;
    let scales: [Option<Calibration>; 4] = core::array::from_fn(|i| {
        offsets[i].and_then(|zero| Calibration::new(zero, COUNTS_PER_KG[i]).ok())
    });
    write_serial(
        serial,
        "Tare complete. 'kg~' is NOMINAL, not calibrated.\r\n",
    )
    .await?;

    loop {
        for (i, sensor) in sensors.iter_mut().enumerate() {
            let mut line: String<128> = String::new();
            match sensor.read().await {
                Ok(raw) => match scales[i] {
                    Some(scale) => {
                        write!(
                            line,
                            "LC{} raw={} delta={} kg~={:.3}\r\n",
                            i + 1,
                            raw,
                            scale.delta(raw),
                            scale.kilograms(raw)
                        )
                        .expect("line buffer capacity");
                    }
                    None => {
                        write!(line, "LC{} raw={} status=no_tare\r\n", i + 1, raw)
                            .expect("line buffer capacity");
                    }
                },
                Err(error) => {
                    let message = match error {
                        ReadError::Timeout => "timeout",
                        ReadError::Saturated => "saturated",
                    };
                    write!(line, "LC{} status={}\r\n", i + 1, message)
                        .expect("line buffer capacity");
                }
            }
            write_serial(serial, line.as_str()).await?;
        }
        // HX711 modules are 10 SPS. Each read waits for DOUT LOW.
        Timer::after_millis(5).await;
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
    spawner.spawn(usb_task(builder.build()).expect("USB task allocation"));

    // Let the rails and the HX711s settle before enabling the translator.
    Timer::after_millis(100).await;
    oe.set_low();
    Timer::after_millis(500).await;

    loop {
        // Endpoint enablement only means enumeration, not an open terminal.
        serial.wait_connection().await;
        while !control.dtr() {
            control.control_changed().await;
        }
        match select(
            wait_disconnect(&control),
            run_measurements(&mut serial, &mut sensors),
        )
        .await
        {
            Either::Second(Err(EndpointError::BufferOverflow)) => {
                panic!("USB packet exceeds endpoint capacity");
            }
            Either::First(()) | Either::Second(Ok(()) | Err(EndpointError::Disabled)) => {}
        }
    }
}
