# Pico 2 W / Rust / 4 load cells

Pico 2 W (RP2350) + 4x Akizuki AE-HX711-SIP + 4x SC134-50kg-CTH
+ Akizuki FXMA108 level translator.

- Native USB CDC-ACM or BLE notifications; no USB-UART adapter or Wi-Fi connection required.
- No Arduino core; all firmware is Rust (`no_std`).
- Platform-independent 24-bit decoding/calibration in `loadcell-core`.
- Independent asynchronous acquisition and per-sensor output queues in
  `loadcell-acquisition` (`no_std`, hardware-independent).
- Includes timeouts, saturated sample detection and DOUT handshake checks.
- Firmware outputs validated raw ADC counts only. Zero offsets and signed weight calibration live in the PC monitor; nominal sensitivities are not verified calibration.
- Experimental USB VID/PID, not intended for deployment as a commercial USB device.

## 接続先の自動選択とステータスLED

USBのCOMポートが開かれてDTRが有効ならUSBへ送信する。それ以外では、
BLEクライアントが通知を購読していればBLEへ送信する。両方が利用可能な場合は
USBを優先する。USBケーブルが電源供給のためにつながっているだけならBLEを使える。
未接続時は測定セッションを開始せず、接続を待つ。

切り替え・再接続のたびに古い送信待ちデータを破棄し、各HX711をリセットして
500 msの安定待ち後にraw値の送信を始める。マイコンではゼロ点調整・重量換算を行わない。
モニターの **Auto zero on connection** が有効なら、接続後と新しい測定セッションで
PC側のゼロ点を取得する。**この設定が有効な間は、接続・切り替え時に測定対象を取り除く。**
空の測定台は載せたままにする。既存のゼロ点を保持して測定する場合は自動調整を無効にする。
BLE接続が残っていれば、USBのモニターを閉じた際にBLEで測定を再開する。
センサー4台の非同期読み取りは一つのタスク群が所有し、送信待ちと並行して進める。

Pico 2 Wの内蔵LED（CYW43439のGPIO 0）は次の状態を示す。

| 状態 | 表示 |
|---|---|
| 接続待ち | 1秒点灯・1秒消灯 |
| センサー初期化中 | 250 ms点灯・250 ms消灯 |
| 4台とも正常に測定中 | 常時点灯 |
| 読み取り異常・BLEホスト異常 | 100 ms点灯・100 ms消灯 |

センサーを一部だけ接続した試験では、未接続チャンネルのタイムアウトにより速い点滅になる。
モニターで表示を隠す設定は、マイコンの異常判定対象を変更しない。
LEDはCYW43初期化後に動作する。起動前や無線チップの初期化自体が停止した状態を
LEDだけで区別することはできない。
PC側のゼロ点調整状態はモニターに表示し、マイコンのLEDには反映しない。

BLEは専用GATTサービス `854b6df0-3908-4ae5-8862-89c122e17401`、
通知特性 `854b6df1-3908-4ae5-8862-89c122e17401`、名前 `Pico Loadcells` を使う。
この実装はペアリング・暗号化を行わない測定用接続で、同時BLE接続は1台。
PCモニターの `BLE` → `Connect` で5秒間検索し、該当する1台に接続する。
複数台見つかった場合は表示されたアドレスを `BLE address` に入力して再接続する。
Windows設定での事前ペアリングや仮想COMポートは不要。

通知はATT MTUに合わせて分割する。各通知の先頭は連番u16（little-endian）と
フラグ1バイト（bit 0: メッセージ先頭、bit 1: 末尾）、残りが従来のUTF-8ログ。
最大通知長125バイト、最大メッセージ長512バイト。PC側は連番欠落時に途中の
メッセージを破棄し、グラフ・重心軌跡に欠測区間を作る。
BLEの実効速度はPCと接続条件に依存する。80 SPS×4台での無欠落は未確認であり、
画面の有効サンプル数、`Dropped LCn`、欠落診断で確認する。

無線チップ用バイナリは [firmware/cyw43-firmware](firmware/cyw43-firmware) に同梱し、
Embassyの固定コミット、SHA-256、配布ライセンスを記録している。
参照: [Pico 2 Wデータシート](https://datasheets.raspberrypi.com/picow/pico-2-w-datasheet.pdf)、
[採用版TrouBLEの公式サンプル](https://github.com/embassy-rs/trouble/blob/29b0c515831e4be2bba5e55616bfa6297d75db31/examples/apps/src/ble_bas_peripheral.rs)、
[btleplugの通知受信例](https://github.com/deviceplug/btleplug/blob/master/examples/subscribe_notify_characteristic.rs)。

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

Open the **new virtual COM port** in the PC monitor (115200 bps, DTR enabled),
or choose BLE and subscribe. The firmware emits a session banner, resets each
HX711 to A/128, waits 500 ms per sensor asynchronously, and streams raw counts.
There is no three-second countdown, tare averaging, stored zero or kg conversion
on the MCU. Reset, timing guards, timeouts and per-sensor recovery remain active.
Use the monitor's PC zero controls for automatic or manual tare; keep the empty
platform still and remove the object whenever capturing zero.

## Independent four-sensor acquisition

- Four acquisition futures wait for their own DOUT independently. A sensor's
  300 ms timeout never prevents the others from reading their next conversion
  during streaming. Each sensor starts streaming as soon as its first valid conversion is available.
- All futures run on one executor/core. The interrupt-masked 25-pulse GPIO
  transactions execute one at a time (102 us of requested delays plus software
  overhead, not a measured duration); the clocks are
  not synchronized and conversions are not simultaneous triggered samples.
- Each source has a 1 ms asynchronous pacing delay before its next read, so a
  stuck-low input cannot continuously occupy the CPU. Healthy devices wait
  for their own ADC conversion, including while the other sensors are read.
- USB transmission runs concurrently with acquisition. Each sensor has a
  separate 16-entry queue, drained fairly. A full queue drops its oldest entry
  without stopping acquisition; `Dropped LC1 samples=N (transport backlog)` reports
  the loss. This diagnostic appears in the monitor as text, not as a sample.
- Queues and all acquisition futures belong to the USB/DTR session. Disconnect
  cancels pending waits and drops queued data; reconnect starts fresh raw acquisition.
- USB lifecycle events are monitored separately from DTR. The RP driver does
  not reliably return `EndpointError::Disabled` from a blocked write. Reset,
  reconfiguration and suspend invalidate the session even if DTR remains HIGH;
  a generation counter preserves that boundary across rapid reset/resume.
  Each new banner begins with CRLF to terminate a previously interrupted line.
  Already submitted USB/host bytes cannot be recalled by cancelling a future.
  RP VBUS removal detection is not implemented in this driver: with external
  power, cable removal is handled when suspend/reset is reported, not by a
  guaranteed immediate VBUS event. This still requires a physical test.
- The new sample format is `LCn raw=N`; fault formats are unchanged. Lines arrive as individual
  readings become available, rather than in strict LC1-LC4 batches. Monitor
  timestamps remain PC reception times, not ADC timestamps.
- Standard AE-HX711-SIP boards remain nominally 10 SPS per sensor. Parallel
  acquisition removes cross-sensor waiting; it does not change the RATE pin.
- `status=protocol` means DOUT was no longer LOW before clocking, failed
  to return HIGH after pulse 25, or a measured clock-HIGH interval was >=50 us.
  Such readings are reported as faults rather than raw measurements.
  An explicit 2 us delay before the first clock provides the T1 setup margin.
  These checks do not provide a CRC or detect every corrupted 24-bit word.
- After a protocol error only that sensor is reset before its next read and
  waits 500 ms asynchronously. The reset restores A/128 after a partial word;
  the settling wait also limits repeated errors from a stuck-low input.
  Cancellation during settling never marks the sensor ready prematurely.
- Signed counts/kg now belong to the PC monitor. Its initial LC1 sensitivity is
  negative, matching the observed compression direction; calibrate every sensor.
  Negative raw, zero and `raw=-1` remain valid ADC words unless the protocol
  checks reject the transaction. No envelope or sign-based filtering is applied.

Sample serial output (numbers are illustrative):

```
Pico 2 W / HX711 x4
Session started. Raw output; zero and calibration are PC-side.
LC1 raw=-219113
LC2 raw=-553920
LC3 raw=893012
LC4 status=timeout
```

## PC serial monitor and plotter

For simultaneous raw-text monitoring and live LC1-LC4 graphs, see the
[PC serial plotter](tools/serial-plotter/README.md). From PowerShell, launch it
with `.\tools\serial-plotter\run.ps1 -Port COM5`. Close other serial monitors
before connecting. Only LC1 is selected for plotting by default.

## Calibrating accurately

This exact load cell is nominally 1.8 mV/V at 50 kg; at HX711 gain 128,
the theoretical sensitivity is approximately 77,309 ADC counts/kg.
Real sensitivity and mounting conditions vary. For each sensor:

1. In the PC monitor, remove the load from the individual sensor and capture its zero.
2. Apply a known mass to that sensor only, then use `Capture span (auto polarity)`.
3. Repeat for the other sensors. Signed sensitivity is `(raw_loaded - raw_zero)/mass`.
4. Assemble the platform, remove the object, and capture the empty-platform zero.
5. Save settings in the PC monitor. No firmware rebuild is needed for calibration.

The monitor averages recent healthy raw samples for zero capture. Automatic PC
zero can be disabled to reuse a saved offset when reconnecting under load.

**Caution:** the SC134's 50 kg rating applies per sensor. Mounting geometry,
eccentric force, bending and overload can corrupt results or damage hardware.
The Akizuki HX711 module drives the excitation via the on-board regulator
(approximately 4.3V on a 5V supply); check whether that satisfies the chosen
sensor's datasheet, as Akizuki's product listing says 5-10V while a manufacturer
sheet describes a wider range.

## Datasheet audit (2026-10-30)

The checked hardware is the README configuration; the actual PCB revisions,
chip stepping, solder joints and waveforms have not been inspected.

| Area | Finding / action |
|---|---|
| HX711 transaction | MSB-first 24-bit signed decoding, 25 clocks for A/128, saturation codes and 2 us HIGH/LOW delay requests agree with the chip sheet. Added a ready recheck, explicit T1 delay and post-clock DOUT check; previously a stuck-low input could produce accepted zeros. |
| Session initialization | Reset and 500 ms settling precede raw acquisition; zero is now PC-side. The former code assumed a powered HX711 was already in the correct state and could include an old pending conversion in the offset. |
| Error recovery | A failed handshake or clock timing check schedules a per-sensor reset plus 500 ms settling before the next read. Previously repeated errors retried a possibly desynchronized ADC at roughly 1 ms intervals. |
| Clock timing | Sample data with PD_SCK LOW, following the manufacturer's reference driver. Measure all 25 HIGH intervals; reject >=50 us conservatively and leave the clock LOW on failure. The monitor detects overruns rather than preventing them. |
| Zero capture | MCU tare was removed. PC captures the enabled sensors atomically from fresh, uninterrupted raw windows; unavailable sensors prevent a partial update. |
| Four sensors | Separate DOUT waits and queues; no peer timeout blocks streaming. Transactions share one CPU and are not synchronized ADC conversions. |
| USB lifecycle | End acquisition on reset, configuration changes and suspend, independently of DTR and endpoint error returns. Resume starts a new raw session; rapid events cannot reuse an old session. |
| HX711 board | CN1/CN2/CN3 mapping and J3/J4 closed, J1/J2/J5/J6 open match the board manual. RATE is wired LOW: nominally 10 SPS, not 80 SPS through a software setting. |
| FXMA108 | VA=3.3 V, VB=5 V, /OE LOW enable and `Pull::None` match the translator requirements. Its bus hold means a disconnected data input need not time out. Do not add pull resistors to active translated data lines as a generic fix. |
| Pico 2 W / RP2350 | GP2-GP10 mapping and ARM target agree. Reserved flash/RAM fit the board. Default Embassy clocks are 150 MHz system / 48 MHz USB, satisfying the RP2350-E12 clock workaround. |
| SC134 | Nominal 77,309 counts/kg is consistent with the 1.8 mV/V sheet and ratiometric AVDD excitation. Older documentation instead gives 1.75 mV/V and a 5 V minimum. Calibrate each unit and identify its applicable specification. |

RP2350-E9 applies to **A2** silicon (the current datasheet lists A3 as fixed).
It can source roughly 120 uA in the undefined input region. FXMA108's weak
static drive / bus hold makes this a hardware compatibility concern to check,
not a proven cause of this installation's spikes. Check the chip stepping and
observe DOUT on both sides of the translator. A blanket 8.2 kohm pull-down
workaround conflicts with FXMA108's no-pull recommendation and was not added.

On hardware, measure normal PD_SCK HIGH <=50 us (a HIGH >60 us intentionally
powers the HX711 down), DOUT before the first clock and after clock 25, and
VA/VB/AVDD. The tests verify requested delays, ordering, recovery/cancellation
and rejection of injected timing overruns, not electrical pulse widths or a
hard execution-time bound. The timing guard uses the MCU timer and cannot
observe distortions after FXMA108. A debugger stopping with PD_SCK HIGH can
still reset the ADC; timer behavior while halted is not certified. Negative kg and `raw=-1` alone are not
proof of a read error; do not use an envelope as a substitute for this check.

Sources read (including the module schematics and HX711 timing diagram):

- [HX711 chip datasheet, pp. 3-5](https://akizukidenshi.com/goodsaffix/hx711.pdf)
- [AE-HX711-SIP manual, both pages](https://akizukidenshi.com/goodsaffix/AE-HX711-SIP_web_20231219.pdf)
- [FXMA108 datasheet, pp. 4-6](https://www.onsemi.com/pdf/datasheet/fxma108-d.pdf)
- [AE-LLCNV8-DIP20 schematic](https://akizukidenshi.com/goodsaffix/AE-LLCNV8-DIP20_sch_20220110.pdf)
- [Pico 2 W datasheet, pinout and power](https://datasheets.raspberrypi.com/picow/pico-2-w-datasheet.pdf)
- [RP2350 datasheet, errata D.5.1 / D.11.1](https://datasheets.raspberrypi.com/rp2350/rp2350-datasheet.pdf)
- [SC134 1.8 mV/V / 3-10 V sheet](https://akizukidenshi.com/goodsaffix/SC134-50kg-CTH%2C%20%20Full%20Bridge.pdf), [older 1.75 mV/V / 5-10 V sheet](https://akizukidenshi.com/goodsaffix/SC134-50kgCTH.pdf)
- [Embassy USB lifecycle handler API](https://docs.rs/embassy-usb/0.6.0/embassy_usb/trait.Handler.html), checked against the installed 0.6.0 source and RP 0.10.0 endpoint implementation. [Upstream disconnect report](https://github.com/embassy-rs/embassy/issues/6383) concerns RP2040; this project's RP2350 hardware behavior remains unverified.

Audit validation: 23 firmware/core host tests (including the actual wire
transaction, recovery and USB-session cancellation), ARM release/dev builds
and Clippy passed. The preceding monitor audit's 24 tests passed; this firmware
follow-up keeps the measurement and diagnostic formats unchanged. The
hardware-independent protocol now lives in
`loadcell-acquisition/src/hx711.rs` and is used directly by firmware and tests. Existing
Verus arithmetic code was unchanged; GPIO/USB behavior is outside those proofs.
No hardware flash, physical timing capture or four-device measurement was done.

## Validation status

- Source pin map and protocols checked against manufacturer documentation.
- RP2350 memory layout and image-definition placement supplied via `firmware/memory.x` and `firmware/build.rs`.
- `loadcell-core` includes unit tests intended for `cargo test -p loadcell-core --target <host triple>`.
- RP2350 ARM release build and UF2 generation succeeded with the Verus-annotated
  core. UF2 family ID, boot image definition, vector table, and ELF payload
  consistency were checked.
- Host core tests (4 unit tests and 1 doctest), Clippy with `-D warnings`, and
  formatting passed.
- Verus 0.2026.10.04.426d8b0 verified the actual core implementation:
  `10 verified, 0 errors`. Deliberately corrupting the decoder is rejected
  by its postconditions.
- The dependency `proc-macro-error2 2.0.1` emits a Rust future-incompatibility
  warning; the current build succeeds and the warning remains visible.
- Firmware **has not been exercised on hardware in this environment**.
- The independent-acquisition change is covered by host tests of the actual
  orchestration: pending peers, concurrent full-sample tare, stalled USB/overflow/fairness,
  and cancellation with a fresh session. GPIO timing and physical four-sensor
  throughput still require hardware verification; these async routines are
  outside the existing Verus arithmetic proofs.
- The repo's global ARM target can be overridden on a host with:
  `cargo test -p loadcell-core --target x86_64-pc-windows-msvc` (Windows MSVC),
  or `cargo test -p loadcell-core --target x86_64-unknown-linux-gnu` (Linux).

Test both hardware-independent crates on Windows:

```powershell
cargo test --locked -p loadcell-core -p loadcell-acquisition --target x86_64-pc-windows-msvc
```

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
cargo +1.98.1 verus verify -p loadcell-core --locked --target x86_64-pc-windows-msvc
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
