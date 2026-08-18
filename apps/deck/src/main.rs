use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui;
use sdft_core::DEFAULT_PORT;
use sdft_core::receiver::{ReceiveEvent, listen};

fn main() {
    let result = if env::args_os().len() > 1 {
        run_cli()
    } else {
        run_gui()
    };
    if let Err(error) = result {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn run_gui() -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 440.0])
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Steam Deck File Transfer - Receiver",
        options,
        Box::new(|_context| Ok(Box::<DeckApp>::default())),
    )?;
    Ok(())
}

enum WorkerMessage {
    Event(ReceiveEvent),
    Done(String),
}

struct DeckApp {
    address: String,
    output: String,
    status: String,
    current_file: String,
    progress: f32,
    worker: Option<Receiver<WorkerMessage>>,
}

impl Default for DeckApp {
    fn default() -> Self {
        Self {
            address: format!("0.0.0.0:{DEFAULT_PORT}"),
            output: default_output().display().to_string(),
            status: "Choose a receive folder, then start the receiver.".to_owned(),
            current_file: String::new(),
            progress: 0.0,
            worker: None,
        }
    }
}

impl eframe::App for DeckApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        self.poll_worker(&context);
        egui::CentralPanel::default().show(root, |ui| {
            ui.heading("Receive from PC");
            ui.label("Keep this window open while transferring files.");
            ui.add_space(14.0);

            ui.horizontal(|ui| {
                ui.label("Listen on");
                ui.add_enabled(
                    self.worker.is_none(),
                    egui::TextEdit::singleline(&mut self.address).desired_width(230.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Save to  ");
                ui.add_enabled(
                    self.worker.is_none(),
                    egui::TextEdit::singleline(&mut self.output).desired_width(440.0),
                );
            });

            ui.add_space(14.0);
            if self.worker.is_none() {
                if ui
                    .add_sized([180.0, 42.0], egui::Button::new("Start receiver"))
                    .clicked()
                {
                    self.start_receiver();
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.strong("Receiver is running");
                });
            }

            ui.add_space(18.0);
            ui.separator();
            ui.add_space(12.0);
            if !self.current_file.is_empty() {
                ui.label(&self.current_file);
            }
            ui.add(egui::ProgressBar::new(self.progress).animate(self.worker.is_some()));
            ui.label(&self.status);

            ui.add_space(18.0);
            ui.small("Alpha: accepts transfers from the trusted local network without pairing.");
        });
    }
}

impl DeckApp {
    fn start_receiver(&mut self) {
        let address = match self.address.parse::<SocketAddr>() {
            Ok(address) => address,
            Err(error) => {
                self.status = format!("Invalid listen address: {error}");
                return;
            }
        };
        let output = PathBuf::from(self.output.trim());
        if self.output.trim().is_empty() {
            "Choose a receive folder first.".clone_into(&mut self.status);
            return;
        }

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let event_sender = sender.clone();
            if let Err(error) = listen(address, &output, move |event| {
                let _ = event_sender.send(WorkerMessage::Event(event));
            }) {
                let _ = sender.send(WorkerMessage::Done(error.to_string()));
            }
        });
        "Starting receiver…".clone_into(&mut self.status);
        self.worker = Some(receiver);
    }

    fn poll_worker(&mut self, context: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(worker) = &self.worker {
            while let Ok(message) = worker.try_recv() {
                messages.push(message);
            }
        }
        for message in messages {
            match message {
                WorkerMessage::Event(event) => self.handle_event(event),
                WorkerMessage::Done(error) => {
                    self.worker = None;
                    self.status = format!("Receiver stopped: {error}");
                }
            }
        }
        if self.worker.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn handle_event(&mut self, event: ReceiveEvent) {
        match event {
            ReceiveEvent::Listening(bound) => {
                self.status = format!("Ready on {bound}. Enter this Deck's LAN IP on the PC.");
            }
            ReceiveEvent::Connected(peer) => self.status = format!("Connection from {peer}"),
            ReceiveEvent::TransferOffered { files, bytes } => {
                self.status = format!("Incoming: {files} file(s), {}", format_bytes(bytes));
            }
            ReceiveEvent::FileStarted { path, size } => {
                self.current_file = format!("{} — {}", path.display(), format_bytes(size));
            }
            ReceiveEvent::Progress { received, total } => {
                self.progress = progress_fraction(received, total);
                self.status = format!("{} / {}", format_bytes(received), format_bytes(total));
            }
            ReceiveEvent::FileCompleted { path } => {
                self.status = format!("Verified {}", path.display());
            }
            ReceiveEvent::Failed { message } => self.status = message,
            ReceiveEvent::Complete { files, bytes } => {
                self.progress = 1.0;
                self.status = format!(
                    "Complete: {files} file(s), {} verified",
                    format_bytes(bytes)
                );
            }
        }
    }
}

fn run_cli() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let mut address = SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT));
    let mut output = default_output();
    while let Some(argument) = arguments.next() {
        if argument == "--listen" || argument == "-l" {
            let value = arguments
                .next()
                .ok_or("--listen requires an IP:port value")?;
            address = value
                .to_str()
                .ok_or("listen address is not valid Unicode")?
                .parse()?;
        } else if argument == "--output" || argument == "-o" {
            output = PathBuf::from(
                arguments
                    .next()
                    .ok_or("--output requires a directory path")?,
            );
        } else if argument == "--help" || argument == "-h" {
            print_help();
            return Ok(());
        } else {
            return Err(format!("unknown argument: {}", argument.to_string_lossy()).into());
        }
    }

    println!("WARNING: this alpha build is not encrypted; use it only on a trusted LAN.");
    println!("Receive folder: {}", output.display());
    let mut last_percent = None;
    listen(address, &output, |event| match event {
        ReceiveEvent::Listening(bound) => println!("Receiver ready on {bound}"),
        ReceiveEvent::Connected(peer) => println!("Connection from {peer}"),
        ReceiveEvent::TransferOffered { files, bytes } => {
            println!("Incoming: {files} file(s), {}", format_bytes(bytes));
        }
        ReceiveEvent::FileStarted { path, size } => {
            println!("Receiving {} ({})", path.display(), format_bytes(size));
        }
        ReceiveEvent::Progress { received, total } => {
            let percent = received
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100);
            if last_percent != Some(percent) {
                println!(
                    "Progress: {percent}% ({}/{})",
                    format_bytes(received),
                    format_bytes(total)
                );
                last_percent = Some(percent);
            }
        }
        ReceiveEvent::FileCompleted { path } => println!("Verified {}", path.display()),
        ReceiveEvent::Failed { message } => eprintln!("{message}"),
        ReceiveEvent::Complete { files, bytes } => {
            println!(
                "Complete: {files} file(s), {} received",
                format_bytes(bytes)
            );
            last_percent = None;
        }
    })
    .map_err(Into::into)
}

fn default_output() -> PathBuf {
    env::var_os("HOME").map_or_else(
        || PathBuf::from("SteamDeckFileTransfer"),
        |home| {
            PathBuf::from(home)
                .join("Downloads")
                .join("SteamDeckFileTransfer")
        },
    )
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn progress_fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64) as f32
    }
}

#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn print_help() {
    println!(
        "Steam Deck File Transfer - Deck receiver\n\n\
         Launch without arguments for the graphical interface.\n\n\
         CLI usage:\n  sdft-deck [--listen IP:PORT] [--output DIRECTORY]"
    );
}
