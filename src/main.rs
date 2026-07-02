use eframe::egui;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;

const SOURCE_DIR: &str = "/SNS/VENUS/shared/software/git/python_notebooks";
const COPY_FOLDERS: &[&str] = &["notebooks", "static", "util"];
const COPY_FILES: &[&str] = &["README.md", "pixi.toml"];
const LAUNCH_SCRIPT: &str = "/SNS/VENUS/shared/software/scripts/start_jupyter_pixi_via_rust";
/// Maintenance script that lists and kills stuck browser / Jupyter sessions
/// across the shared analysis machines (same one the marimo portal uses).
const FIX_BROWSER_SCRIPT: &str =
    "/SNS/VENUS/shared/software/bin/list_and_fix_running_browser.sh";
const LOGO_BYTES: &[u8] = include_bytes!("../logos/ImagingLogo.png");
const LOGO_MAX_HEIGHT: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Instrument {
    Venus,
    Mars,
}

impl Instrument {
    fn label(self) -> &'static str {
        match self {
            Instrument::Venus => "VENUS",
            Instrument::Mars => "MARS",
        }
    }

    fn root(self) -> &'static Path {
        match self {
            Instrument::Venus => Path::new("/SNS/VENUS"),
            Instrument::Mars => Path::new("/HFIR/CG1D"),
        }
    }
}

#[derive(Clone)]
struct Entry {
    label: String,
    path: PathBuf,
    writable: bool,
}

fn can_access(path: &Path) -> bool {
    let Ok(cstr) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(cstr.as_ptr(), libc::R_OK | libc::X_OK) == 0 }
}

fn can_write(path: &Path) -> bool {
    let Ok(cstr) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(cstr.as_ptr(), libc::W_OK) == 0 }
}

fn home_entry() -> Option<Entry> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home);
    if !can_access(&path) {
        return None;
    }
    let writable = can_write(&path);
    Some(Entry {
        label: "HOME".to_string(),
        path,
        writable,
    })
}

fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(LOGO_BYTES).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    Some(ctx.load_texture("imaging_logo", color_image, egui::TextureOptions::LINEAR))
}

fn user_id() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".to_string())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            if dst_path.exists() {
                let _ = fs::remove_file(&dst_path);
            }
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn destination_for(entry: &Entry, instrument: Instrument) -> Option<PathBuf> {
    if entry.label == "HOME" {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("notebooks").join("python_notebooks"))
    } else if entry.label.starts_with("IPTS-") {
        Some(
            instrument
                .root()
                .join(&entry.label)
                .join("shared")
                .join("notebooks")
                .join(format!("imaging_notebooks_{}", user_id())),
        )
    } else {
        None
    }
}

fn provision_and_launch(dest: &Path) -> Result<(), String> {
    let src = Path::new(SOURCE_DIR);
    fs::create_dir_all(dest)
        .map_err(|e| format!("create {}: {e}", dest.display()))?;
    for folder in COPY_FOLDERS {
        let s = src.join(folder);
        let d = dest.join(folder);
        copy_dir_recursive(&s, &d)
            .map_err(|e| format!("copy {} -> {}: {e}", s.display(), d.display()))?;
    }
    for file in COPY_FILES {
        let s = src.join(file);
        let d = dest.join(file);
        if d.exists() {
            let _ = fs::remove_file(&d);
        }
        fs::copy(&s, &d)
            .map_err(|e| format!("copy {} -> {}: {e}", s.display(), d.display()))?;
    }
    Command::new(LAUNCH_SCRIPT)
        .arg(dest)
        .spawn()
        .map_err(|e| format!("spawn {LAUNCH_SCRIPT}: {e}"))?;
    Ok(())
}

/// Kick off the browser-cleanup script and return a channel that fires when
/// it exits, so the UI can clear its status message.
fn launch_fix_browser() -> Result<mpsc::Receiver<()>, String> {
    let mut child = Command::new(FIX_BROWSER_SCRIPT)
        .arg("kill")
        .spawn()
        .map_err(|e| format!("spawn {FIX_BROWSER_SCRIPT}: {e}"))?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = child.wait();
        let _ = tx.send(());
    });
    Ok(rx)
}

fn list_entries(root: &Path) -> Result<Vec<Entry>, String> {
    let mut out: Vec<Entry> = Vec::new();
    if let Some(home) = home_entry() {
        out.push(home);
    }

    let dir = fs::read_dir(root).map_err(|e| format!("Cannot read {}: {e}", root.display()))?;
    let mut ipts: Vec<(u64, Entry)> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Some(suffix) = name_str.strip_prefix("IPTS-") else {
            continue;
        };
        let path = entry.path();
        if !can_access(&path) {
            continue;
        }
        let writable = can_write(&path.join("shared"));
        let num: u64 = suffix.parse().unwrap_or(u64::MAX);
        ipts.push((
            num,
            Entry {
                label: name_str.into_owned(),
                path,
                writable,
            },
        ));
    }
    ipts.sort_by_key(|(n, _)| *n);
    out.extend(ipts.into_iter().map(|(_, e)| e));
    Ok(out)
}

struct App {
    instrument: Instrument,
    loaded_for: Option<Instrument>,
    entries: Vec<Entry>,
    selected: Option<usize>,
    error: Option<String>,
    launch_status: Option<Result<String, String>>,
    launching: bool,
    pending_launch: Option<PathBuf>,
    logo: Option<egui::TextureHandle>,
    manual_ipts: String,
    manual_ipts_msg: Option<(String, egui::Color32)>,
    scroll_to_selected: bool,
    fix_browser_status: Option<(String, egui::Color32)>,
    /// Signals when the cleanup script exits, so the message can be cleared.
    fix_browser_rx: Option<mpsc::Receiver<()>>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            instrument: Instrument::Venus,
            loaded_for: None,
            entries: Vec::new(),
            selected: None,
            error: None,
            launch_status: None,
            launching: false,
            pending_launch: None,
            logo: None,
            manual_ipts: String::new(),
            manual_ipts_msg: None,
            scroll_to_selected: false,
            fix_browser_status: None,
            fix_browser_rx: None,
        }
    }
}

impl App {
    fn refresh(&mut self) {
        match list_entries(self.instrument.root()) {
            Ok(list) => {
                self.entries = list;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
            }
        }
        self.selected = None;
        self.launch_status = None;
        self.launching = false;
        self.pending_launch = None;
        self.manual_ipts.clear();
        self.manual_ipts_msg = None;
        self.scroll_to_selected = false;
        self.loaded_for = Some(self.instrument);
    }

    fn ipts_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.label.starts_with("IPTS-"))
            .count()
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(dest) = self.pending_launch.take() {
            match provision_and_launch(&dest) {
                Ok(()) => {
                    self.launch_status = Some(Ok(format!("Launched: {}", dest.display())));
                }
                Err(e) => {
                    self.launching = false;
                    self.launch_status = Some(Err(e));
                }
            }
        }

        if self.loaded_for != Some(self.instrument) {
            self.refresh();
        }
        if self.logo.is_none() {
            self.logo = load_logo(ctx);
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Instrument:")
                        .strong()
                        .size(18.0),
                );
                let prev = self.instrument;
                egui::ComboBox::from_id_source("instrument")
                    .selected_text(
                        egui::RichText::new(self.instrument.label())
                            .strong()
                            .size(16.0),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.instrument, Instrument::Venus, "VENUS");
                        ui.selectable_value(&mut self.instrument, Instrument::Mars, "MARS");
                    });
                if prev != self.instrument {
                    self.refresh();
                }

                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if let Some(tex) = &self.logo {
                            ui.add(
                                egui::Image::from_texture(tex).max_height(LOGO_MAX_HEIGHT),
                            );
                        }
                    },
                );
            });
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            ui.label(format!(
                "You have access to {} IPTS for this instrument",
                self.ipts_count()
            ));
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("IPTS-").strong());
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.manual_ipts)
                        .desired_width(120.0)
                        .hint_text("number"),
                );
                if resp.changed() {
                    let trimmed = self.manual_ipts.trim().to_string();
                    if trimmed.is_empty() {
                        self.manual_ipts_msg = None;
                    } else if trimmed.parse::<u64>().is_err() {
                        self.manual_ipts_msg =
                            Some(("Enter digits only".to_string(), egui::Color32::RED));
                    } else {
                        let target = format!("IPTS-{}", trimmed);
                        let found = self
                            .entries
                            .iter()
                            .enumerate()
                            .find(|(_, e)| e.label == target)
                            .map(|(idx, e)| (idx, e.writable));
                        match found {
                            Some((idx, true)) => {
                                if self.selected != Some(idx) {
                                    self.launch_status = None;
                                    self.launching = false;
                                    self.pending_launch = None;
                                }
                                self.selected = Some(idx);
                                self.scroll_to_selected = true;
                                self.manual_ipts_msg = None;
                            }
                            Some((_, false)) => {
                                self.manual_ipts_msg = Some((
                                    format!("{} found but no write access", target),
                                    egui::Color32::from_rgb(200, 120, 0),
                                ));
                            }
                            None => {
                                self.manual_ipts_msg = Some((
                                    format!("{} not found", target),
                                    egui::Color32::RED,
                                ));
                            }
                        }
                    }
                }
            });
            if let Some((msg, color)) = &self.manual_ipts_msg {
                ui.colored_label(*color, msg);
            }
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            let selected_entry = self.selected.and_then(|i| self.entries.get(i)).cloned();
            let dest = selected_entry
                .as_ref()
                .and_then(|e| destination_for(e, self.instrument));

            match (&selected_entry, &dest) {
                (Some(entry), Some(d)) => {
                    ui.label(format!("Selected: {}", entry.path.display()));
                    ui.label(format!("Will provision: {}", d.display()));
                }
                (Some(entry), None) => {
                    ui.label(format!("Selected: {}", entry.path.display()));
                    ui.colored_label(egui::Color32::RED, "Cannot determine destination");
                }
                (None, _) => {
                    ui.label("No IPTS selected");
                }
            }

            if let Some(status) = &self.launch_status {
                match status {
                    Ok(msg) => ui.colored_label(egui::Color32::from_rgb(46, 160, 67), msg),
                    Err(msg) => ui.colored_label(egui::Color32::RED, msg),
                };
            }

            // Clear the cleanup message once the script has finished.
            if let Some(rx) = &self.fix_browser_rx {
                if rx.try_recv().is_ok() {
                    self.fix_browser_rx = None;
                    self.fix_browser_status = None;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            }

            let enabled = dest.is_some() && !self.launching;
            let label = if self.launching {
                "Imaging notebooks launching ..."
            } else {
                "Launch the imaging notebooks"
            };
            ui.horizontal(|ui| {
                // Left utility: kill stuck browser sessions.
                let fix_button = egui::Button::new(
                    egui::RichText::new("\u{1F527} Fix browser issue")
                        .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(138, 43, 226))
                .rounding(egui::Rounding::same(6.0))
                .min_size(egui::vec2(0.0, 28.0));
                let resp = ui
                    .add(fix_button)
                    .on_hover_text("Kill stuck browser sessions");
                if resp.clicked() {
                    match launch_fix_browser() {
                        Ok(rx) => {
                            self.fix_browser_rx = Some(rx);
                            self.fix_browser_status = Some((
                                "Killing stuck browser sessions\u{2026}".to_string(),
                                egui::Color32::from_rgb(46, 160, 67),
                            ));
                        }
                        Err(e) => {
                            self.fix_browser_rx = None;
                            self.fix_browser_status = Some((e, egui::Color32::RED));
                        }
                    }
                }

                // Primary action fills the rest of the row.
                ui.add_enabled_ui(enabled, |ui| {
                    let mut button =
                        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE));
                    if enabled {
                        button = button.fill(egui::Color32::from_rgb(46, 160, 67));
                    }
                    if ui
                        .add_sized([ui.available_width(), 28.0], button)
                        .clicked()
                    {
                        if let Some(d) = &dest {
                            self.launching = true;
                            self.launch_status = None;
                            self.pending_launch = Some(d.clone());
                            ctx.request_repaint();
                        }
                    }
                });
            });
            if let Some((msg, color)) = &self.fix_browser_status {
                ui.colored_label(*color, msg);
            }
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (idx, entry) in self.entries.iter().enumerate() {
                    let is_selected = self.selected == Some(idx);
                    if entry.writable {
                        let resp = ui.selectable_label(is_selected, &entry.label);
                        if resp.clicked() {
                            if self.selected != Some(idx) {
                                self.launch_status = None;
                                self.launching = false;
                                self.pending_launch = None;
                            }
                            self.selected = Some(idx);
                            self.manual_ipts = entry
                                .label
                                .strip_prefix("IPTS-")
                                .unwrap_or("")
                                .to_string();
                            self.manual_ipts_msg = None;
                        }
                        if is_selected && self.scroll_to_selected {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                    } else {
                        ui.add_enabled(
                            false,
                            egui::SelectableLabel::new(false, &entry.label),
                        )
                        .on_disabled_hover_text("No write access to shared folder");
                    }
                }
                self.scroll_to_selected = false;
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([480.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Neutron IPTS Browser",
        options,
        Box::new(|_cc| Ok(Box::<App>::default())),
    )
}
