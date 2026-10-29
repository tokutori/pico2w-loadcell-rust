# Load Cell Serial Plotter

Pico 2 WのUSBシリアル出力を受信し、生のテキストと測定値のグラフを同時に表示するPC用Rustアプリ。
既定ではLC1だけをグラフに表示する。ファームウェアの変更・書き込みは不要である。

## 起動（Windows / PowerShell）

リポジトリのルートで実行する。

```powershell
.\tools\serial-plotter\run.ps1 -Port COM5
```

初回は依存ライブラリのダウンロードとコンパイルが行われる。RustとWindows用MSVCビルド環境が必要である。
PowerShellのスクリプト実行が制限されている環境では、以下を直接実行できる。

```powershell
cargo run --locked --manifest-path tools/serial-plotter/Cargo.toml --target x86_64-pc-windows-msvc -p serial-plotter -- --port COM5
```

1. VS Code等で開いているCOM5のシリアルモニターを閉じる。
2. LC1を無負荷にして、アプリの **Connect** を押す。
3. 接続時にDTRが有効になり、ファームウェアのゼロ点調整が始まる。モニターに `Tare complete.` が出るまで無負荷を保つ。未接続チャンネルのタイムアウトにより、約18秒かかる場合がある。
4. 荷重を変えると、上側のグラフと下側のシリアルモニターが更新される。

ポートは手入力、または **Available ports** で選択できる。**Refresh** はポート一覧を更新する。
ポートを開けない場合は画面上部に理由を表示する。別アプリによる占有やUSB接続を確認してから再接続する。
自動再接続は行わない。再接続は新しい表示履歴とゼロ点調整を開始する。

実機なしで画面を確認するには、以下を実行するか、切断中に **Sample log** を押す。

```powershell
.\tools\serial-plotter\run.ps1 -Demo
```

デモは提示されたログのraw値を基にした静的なサンプルである。時刻は0.65秒間隔の仮定値であり、実測時刻ではない。画面には **DEMO** と表示する。

## 表示仕様

- シリアルモニターは全チャンネル・起動メッセージ・エラーテキストを表示する。グラフのチャンネル選択はモニターに影響しない。
- `LC1`〜`LC4` の `raw=整数 delta=整数 kg~=有限数` を解析し、3段のグラフに表示する。各グラフの縦軸は独立して自動調整する。
- 横軸はPCでの受信経過時間である。マイコン上の測定時刻ではなく、一度の読み取りに含まれる行は同じ時刻になる。
- `status=timeout` と `status=saturated` は欠測である。数値ゼロへの置換や欠測区間をまたぐ線の補間は行わない。
- `raw=整数 status=no_tare` はrawのみを描画する。deltaとkg~は欠測である。
- `raw=-1`、負値、ゼロ、急変値をそのまま保持する。平滑化・外れ値除外・再校正は行わない。`kg~` はファームウェア出力の未校正の概算値である。
- **Window** は表示時間幅を変更する。**Pause graphs** はグラフの表示時刻を固定し、パン・ズームを可能にする。受信とモニター表示は続く。**Resume graphs** で追従を再開する。
- **Clear history** は表示履歴を消去する。接続やファームウェアのゼロ点は変更しない。
- **Copy retained log** は保持中の受信テキストをクリップボードにコピーする。ログの自動保存は行わない。

保持上限はモニター2,000行、各チャンネル10,000件、受信待ち64チャンク（各最大4,096バイト）である。
古い履歴から破棄されるため、長時間グラフを停止すると停止範囲のデータも保持上限で失われる。
1行は最大4,096バイトで、超過行と不正UTF-8行はモニターに理由を表示し、次の改行で復帰する。
通常のASCII/UTF-8受信行は改行を除いてそのまま表示する。分割受信された行は連結し、改行前の断片もモニターに表示する。
解析できないLC行はグラフに入れず、エラー件数を増やす。受信キューが満杯になった場合は明示的なエラーで切断する。

## 構成・検証

- `core/`：`#![no_std]`、ヒープ割り当てなし。行解析、固定長フレーミング、測定値の型を提供する。
- `gui/src/history.rs`：表示履歴とグラフ系列の生成。GUI・通信ドライバーには依存しない。
- `gui/src/transport.rs`：別スレッドのシリアル受信、DTR、切断処理。
- `gui/src/main.rs`：eguiのウィンドウとグラフ。

独立したCargoワークスペースとロックファイルを持つ。親のARMターゲット設定は継承されるため、PC向けコマンドでは必ず `--target` を指定する。
Linux等では対応するホストターゲットに置き換える。Linux/macOSでのGUI動作は未検証である。

```powershell
cargo test --locked --manifest-path tools/serial-plotter/Cargo.toml --target x86_64-pc-windows-msvc
cargo clippy --locked --manifest-path tools/serial-plotter/Cargo.toml --target x86_64-pc-windows-msvc --all-targets -- -D warnings
cargo fmt --manifest-path tools/serial-plotter/Cargo.toml --all -- --check
cargo check --locked --manifest-path tools/serial-plotter/Cargo.toml -p serial-plotter-core --target thumbv8m.main-none-eabihf
```

APIの参照元：[eframe](https://docs.rs/eframe/0.33.3/eframe/)、[egui_plot](https://docs.rs/egui_plot/0.34.1/egui_plot/)、[serialportのDTR制御](https://docs.rs/serialport/4.10.1/serialport/trait.SerialPort.html#tymethod.write_data_terminal_ready)。
