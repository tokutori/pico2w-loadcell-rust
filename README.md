# Pico 2 W / Rust / 4 load cells

Pico 2 W (RP2350) + 4x Akizuki AE-HX711-SIP + 4x SC134-50kg-CTH
+ Akizuki FXMA108 level translator.

- No Wi-Fi, no USB-UART adapter. Uses native USB CDC-ACM.
- No Arduino core; all firmware is Rust (`no_std`).
- Platform-independent 24-bit decoding/calibration in `loadcell-core`.
- Includes timeouts and saturated sample detection.
- **The kg~ output uses an uncalibrated nominal factor: do NOT treat it as a verified weight.**
- Experimental USB VID/PID, not intended for deployment as a commercial USB device.

## Wiring (Pico GP number, not physical pin number)

| Cell | Pico DAT | Pico CLK | Translator channels |
|---|---|---|---|
| 1 | GP2 (Pin 4) | GP3 (Pin 5) | A0/B0, A1/B1 |
| 2 | GP4 (Pin 6) | GP5 (Pin 7) | A2/B2, A3/B3 |
| 3 | GP6 (Pin 9) | GP7 (Pin 10) | A4/B4, A5/B5 |
| 4 | GP8 (Pin 11) | GP9 (Pin 12) | A6/B6, A7/B7 |

- GP10 (Pin 14) -> translator `/OE` (low=enabled).
- Pico 3V3(OUT) (Pin 36) -> translator VA; Pico VBUS (Pin 40) ->
  translator VB and HX711 boards' CN1-1 VDD (USB powered only).
- Common GND -> translator GND, HX711 boards' CN1-6 GND.
- Load cell: EXC+ -> HX711 CN2-1 AVDD; EXC- -> CN2-2 GND;
  SIG+ -> CN3-2 INPA; SIG- -> CN3-1 INNA.
- HX711 boards: solder bridges J3 and J4, leave J1/J2/J5/J6 open.
- 5 V power **does not** go into Pico GPIO. The converter separates logic domains.

Before powering up, verify the pin-1 orientation against the actual PCBs.

## Build and flash (Windows, PowerShell)

Install stable Rust from https://rustup.rs and the target & UF2 deployer:

```powershell
rustup target add thumbv8m.main-none-eabihf
cargo install --git https://github.com/JoNil/elf2uf2-rs --rev f14bf2d981772cd9fbe5bac33b685719caac1add --locked --force elf2uf2-rs
```

Hold BOOTSEL while connecting Pico 2 W to USB; wait for `RP2350` boot mass
storage mode, then run from the repository root:

```powershell
cargo build --release -p pico2w-loadcells
cargo run --release -p pico2w-loadcells
```

The `.cargo/config.toml` runner deploys via elf2uf2-rs with `rp2350-arm-s`.
The crates.io release 2.2.0 uses an older CLI and does not support this family;
install the pinned official Git revision above. Check that
`elf2uf2-rs deploy --help` lists `rp2350-arm-s`.

To create a UF2 without flashing:

```powershell
elf2uf2-rs convert --family rp2350-arm-s target\thumbv8m.main-none-eabihf\release\pico2w-loadcells target\thumbv8m.main-none-eabihf\release\pico2w-loadcells.uf2
```

Copy the generated UF2 to the `RP2350` drive in BOOTSEL mode.

Open the **new virtual COM port** in VS Code Serial Monitor or another
terminal, select 115200 bps (USB CDC-ACM ignores the actual UART baud rate),
and **enable DTR**. Firmware waits for DTR before printing the prompt.
Remove all loads when the message prompts, and keep unloaded during tare.
Tare runs once per terminal opening (DTR rising); closing the terminal or
resetting USB cancels the session, including pending writes. Opening it again
starts a new tare. If all four HX711s time out, tare can take about 14.4 seconds
after the initial 3-second countdown.

Sample serial output (numbers are illustrative):

```
Pico 2 W / HX711 x4
Remove all loads; tare starts in 3 seconds.
Tare complete. 'kg~' is NOMINAL, not calibrated.
LC1 raw=180234 delta=77600 kg~=1.004
LC2 raw=-553920 delta=0 kg~=0.000
LC3 raw=893012 delta=153900 kg~=1.991
LC4 status=timeout
```

## Calibrating accurately

This exact load cell is nominally 1.8 mV/V at 50 kg; at HX711 gain 128,
the theoretical sensitivity is approximately 77,309 ADC counts/kg.
Real sensitivity and mounting conditions vary. For each sensor:

1. Start the device without load, and record the `raw` value (zero offset).
2. Apply a known mass `m_ref` in kilograms and record `raw_loaded`.
3. Set the corresponding entry of `COUNTS_PER_KG` in `firmware/src/main.rs` to
   `(raw_loaded - raw_zero)/m_ref` rounded to an integer. The value can be
   **negative** if the installation reverses the sense.
4. Reflash; remove all loads and let tare complete again.

When obtaining zero, use the average of multiple samples; the firmware already
performs 12-sample averaging on USB connection. The displayed `delta` is
`raw - zero`, so for `m_ref`, `COUNTS_PER_KG = delta / m_ref` directly.

**Caution:** the SC134's 50 kg rating applies per sensor. Mounting geometry,
eccentric force, bending and overload can corrupt results or damage hardware.
The Akizuki HX711 module drives the excitation via the on-board regulator
(approximately 4.3V on a 5V supply); check whether that satisfies the chosen
sensor's datasheet, as Akizuki's product listing says 5-10V while a manufacturer
sheet describes a wider range.

## Validation status

- Source pin map and protocols checked against manufacturer documentation.
- RP2350 memory layout and image-definition placement supplied via `firmware/memory.x` and `firmware/build.rs`.
- `loadcell-core` includes unit tests intended for `cargo test -p loadcell-core --target <host triple>`.
- RP2350 ARM release build, UF2 generation, host core tests (3 unit tests and
  1 doctest), Clippy with `-D warnings`, and formatting were checked before
  adding the Verus annotations.
- Verus contracts are being added to the actual core implementation; formal
  verification and the final build checks are pending.
- Firmware **has not been exercised on hardware in this environment**.
- The repo's global ARM target can be overridden on a host with:
  `cargo test -p loadcell-core --target x86_64-pc-windows-msvc` (Windows MSVC),
  or `cargo test -p loadcell-core --target x86_64-unknown-linux-gnu` (Linux).

## Formal verification

`loadcell-core` uses Verus annotations in the same functions compiled into
the firmware. `vstd` is pinned with default features disabled to preserve
`no_std` without `alloc`. The contracts cover 24-bit two's-complement decoding,
zero-sensitivity rejection, calibration field preservation, and exact i64
differences for all i32 inputs. Floating-point `kilograms` conversion remains
ordinary Rust outside the verified block.

Install [Verus 0.2026.10.04.426d8b0](https://github.com/verus-lang/verus/releases/tag/release/0.2026.10.04.426d8b0)
and Rust 1.98.1, then add the extracted Verus directory to `PATH`:

```powershell
rustup toolchain install 1.98.1 --profile minimal
cargo +1.98.1 verus verify -p loadcell-core --target x86_64-pc-windows-msvc --locked
```

The verification target is the host; firmware builds continue to use
`thumbv8m.main-none-eabihf`.

References:
- HX711 https://akizukidenshi.com/goodsaffix/AE-HX711-SIP_web_20231219.pdf
- FXMA108 https://www.onsemi.com/pdf/datasheet/fxma108-d.pdf
- Akizuki SC134 https://akizukidenshi.com/catalog/g/g117556/
- Embassy USB CDC https://github.com/embassy-rs/embassy/blob/main/examples/rp/src/bin/usb_serial.rs
- RP2350 rust target / flashing https://github.com/JoNil/elf2uf2-rs

The linker map follows RP2350 startup requirements; the first 2 MiB of flash
are sufficient for this project. `embassy-rp` automatically emits a secure
executable image-definition block for RP2350 when `rp235xa` is enabled.
