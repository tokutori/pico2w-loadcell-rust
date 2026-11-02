#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod axis;
mod balance;
mod history;
mod transport;
use app::Plotter;
use eframe::egui;

const COLORS: [egui::Color32; 4] = [
    egui::Color32::from_rgb(60, 190, 230),
    egui::Color32::from_rgb(250, 185, 70),
    egui::Color32::from_rgb(140, 215, 115),
    egui::Color32::from_rgb(220, 125, 215),
];

fn main() -> eframe::Result {
    let mut port = if cfg!(windows) {
        "COM5"
    } else {
        "/dev/ttyACM0"
    }
    .to_owned();
    let mut demo = false;
    let mut cg_demo = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => demo = true,
            "--cg-demo" => cg_demo = true,
            "--port" => {
                let Some(value) = args.next() else {
                    eprintln!("--port requires a value");
                    std::process::exit(2);
                };
                port = value;
            }
            "--help" | "-h" => {
                println!(
                    "serial-plotter [--port COM5] [--demo | --cg-demo]\nPorts are opened only when Connect is clicked."
                );
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {arg}");
                std::process::exit(2);
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 1000.0])
            .with_min_inner_size([850.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Load Cell Serial Plotter",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut app = Plotter::new(port, false);
            app.initialize(demo, cg_demo);
            Ok(Box::new(app))
        }),
    )
}
