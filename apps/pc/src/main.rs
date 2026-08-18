use std::env;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui;
use sdft_core::DEFAULT_PORT;
use sdft_core::sender::{SendEvent, send_paths};

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
            .with_inner_size([680.0, 520.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Steam Deck File Transfer",
        options,
        Box::new(|_context| Ok(Box::<PcApp>::default())),
    )?;
    Ok(())
}

enum WorkerMessage {
    Event(SendEvent),
    Done(Result<(), String>),
}

struct PcApp {
    host: String,
    path_input: String,
    files: Vec<PathBuf>,
    status: String,
    current_file: String,
    progress: f32,
    worker: Option<Receiver<WorkerMessage>>,
}

impl Default for PcApp {
    fn default() -> Self {
        Self {
            host: String::new(),
            path_input: String::new(),
            files: Vec::new(),
            status: "Drop files or folders into this window.".to_owned(),
            current_file: String::new(),
            progress: 0.0,
            worker: None,
        }
    }
}

impl eframe::App for PcApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        self.accept_dropped_files(&context);
        self.poll_worker(&context);

        egui::CentralPanel::default().show(root, |ui| {
            ui.heading("Send to Steam Deck");
            ui.label("Direct transfer over your local network");
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                ui.label("Deck address");
                ui.add_enabled(
                    self.worker.is_none(),
                    egui::TextEdit::singleline(&mut self.host)
                        .hint_text("192.168.1.42")
                        .desired_width(240.0),
                );
                ui.label(format!("port {DEFAULT_PORT}"));
            });

            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_min_height(115.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(12.0);
                    ui.strong("Drop files or folders here");
                    ui.label("You can also paste a path below.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let add = ui
                            .add_enabled(
                                self.worker.is_none(),
                                egui::TextEdit::singleline(&mut self.path_input)
                                    .hint_text("C:\\Users\\me\\Videos\\movie.mp4")
                                    .desired_width(420.0),
                            )
                            .lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if ui
                            .add_enabled(self.worker.is_none(), egui::Button::new("Add path"))
                            .clicked()
                            || add
                        {
                            self.add_typed_path();
                        }
                    });
                });
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.strong(format!("Queue ({})", self.files.len()));
                if ui
                    .add_enabled(
                        self.worker.is_none() && !self.files.is_empty(),
                        egui::Button::new("Clear"),
                    )
                    .clicked()
                {
                    self.files.clear();
                }
            });
            egui::ScrollArea::vertical()
                .max_height(130.0)
                .show(ui, |ui| {
                    let mut remove = None;
                    for (index, path) in self.files.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(path.display().to_string());
                            if self.worker.is_none() && ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove {
                        self.files.remove(index);
                    }
                });

            ui.add_space(8.0);
            if !self.current_file.is_empty() {
                ui.label(&self.current_file);
            }
            ui.add(egui::ProgressBar::new(self.progress).animate(self.worker.is_some()));
            ui.label(&self.status);
            ui.add_space(8.0);

            let ready =
                self.worker.is_none() && !self.host.trim().is_empty() && !self.files.is_empty();
            if ui
                .add_enabled_ui(ready, |ui| {
                    ui.add_sized([130.0, 36.0], egui::Button::new("Send"))
                })
                .inner
                .clicked()
            {
                self.start_transfer();
            }

            ui.add_space(10.0);
            ui.small("Alpha: traffic is not encrypted yet. Use only on a trusted LAN.");
        });
    }
}

impl PcApp {
    fn accept_dropped_files(&mut self, context: &egui::Context) {
        if self.worker.is_some() {
            return;
        }
        let paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<_>>()
        });
        for path in paths {
            self.add_path(path);
        }
    }

    fn add_typed_path(&mut self) {
        let value = self.path_input.trim();
        if !value.is_empty() {
            self.add_path(PathBuf::from(value));
            self.path_input.clear();
        }
    }

    fn add_path(&mut self, path: PathBuf) {
        if !path.exists() {
            self.status = format!("Path does not exist: {}", path.display());
        } else if !self.files.contains(&path) {
            self.files.push(path);
            "Ready to send.".clone_into(&mut self.status);
        }
    }

    fn start_transfer(&mut self) {
        let address = match resolve_address(self.host.trim()) {
            Ok(address) => address,
            Err(error) => {
                self.status = format!("Invalid Deck address: {error}");
                return;
            }
        };
        let files = self.files.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let event_sender = sender.clone();
            let result = send_paths(address, &files, move |event| {
                let _ = event_sender.send(WorkerMessage::Event(event));
            })
            .map_err(|error| error.to_string());
            let _ = sender.send(WorkerMessage::Done(result));
        });
        self.progress = 0.0;
        "Preparing files…".clone_into(&mut self.status);
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
                WorkerMessage::Done(result) => {
                    self.worker = None;
                    match result {
                        Ok(()) => {
                            self.progress = 1.0;
                            "Transfer complete and verified.".clone_into(&mut self.status);
                        }
                        Err(error) => self.status = format!("Transfer failed: {error}"),
                    }
                }
            }
        }
        if self.worker.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn handle_event(&mut self, event: SendEvent) {
        match event {
            SendEvent::Preparing(path) => {
                self.status = format!("Preparing {}", path.display());
            }
            SendEvent::Connected(peer) => self.status = format!("Connected to {peer}"),
            SendEvent::TransferStarted { files, bytes } => {
                self.status = format!("Sending {files} file(s), {}", format_bytes(bytes));
            }
            SendEvent::FileStarted { path, size } => {
                self.current_file = format!("{} — {}", path.display(), format_bytes(size));
            }
            SendEvent::Progress { sent, total } => {
                self.progress = progress_fraction(sent, total);
                self.status = format!("{} / {}", format_bytes(sent), format_bytes(total));
            }
            SendEvent::Complete { files, bytes } => {
                self.status = format!("{files} file(s), {} verified", format_bytes(bytes));
            }
        }
    }
}

fn run_cli() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let mut host = None;
    let mut inputs = Vec::new();
    while let Some(argument) = arguments.next() {
        if argument == "--host" || argument == "-h" {
            host = Some(
                arguments
                    .next()
                    .ok_or("--host requires an IP address or hostname")?,
            );
        } else if argument == "--help" {
            print_help();
            return Ok(());
        } else {
            inputs.push(PathBuf::from(argument));
        }
    }

    let host = host.ok_or("missing --host; use --help for usage")?;
    if inputs.is_empty() {
        return Err("choose at least one file or folder".into());
    }
    let host = host.to_str().ok_or("host is not valid Unicode")?;
    let address = resolve_address(host)?;

    let mut last_percent = None;
    send_paths(address, &inputs, |event| match event {
        SendEvent::Preparing(path) => println!("Preparing {}", path.display()),
        SendEvent::Connected(peer) => println!("Connected to {peer}"),
        SendEvent::TransferStarted { files, bytes } => {
            println!("Sending {files} file(s), {}", format_bytes(bytes));
        }
        SendEvent::FileStarted { path, size } => {
            println!("Sending {} ({})", path.display(), format_bytes(size));
        }
        SendEvent::Progress { sent, total } => {
            let percent = sent.saturating_mul(100).checked_div(total).unwrap_or(100);
            if last_percent != Some(percent) {
                println!(
                    "Progress: {percent}% ({}/{})",
                    format_bytes(sent),
                    format_bytes(total)
                );
                last_percent = Some(percent);
            }
        }
        SendEvent::Complete { files, bytes } => {
            println!(
                "Complete: {files} file(s), {} sent and verified",
                format_bytes(bytes)
            );
        }
    })?;
    Ok(())
}

fn resolve_address(host: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    if let Ok(address) = host.parse() {
        return Ok(address);
    }
    let mut addresses = (host, DEFAULT_PORT).to_socket_addrs()?;
    addresses
        .next()
        .ok_or_else(|| format!("could not resolve {host}").into())
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
        "Steam Deck File Transfer - PC sender\n\n\
         Launch without arguments for the graphical interface.\n\n\
         CLI usage:\n  sdft-pc --host <DECK-IP[:PORT]> <FILE-OR-FOLDER>..."
    );
}
