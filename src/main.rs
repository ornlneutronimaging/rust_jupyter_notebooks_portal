use eframe::egui;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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
}

fn can_access(path: &Path) -> bool {
    let Ok(cstr) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(cstr.as_ptr(), libc::R_OK | libc::X_OK) == 0 }
}

fn home_entry() -> Option<Entry> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home);
    if !can_access(&path) {
        return None;
    }
    Some(Entry {
        label: "HOME".to_string(),
        path,
    })
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
        let num: u64 = suffix.parse().unwrap_or(u64::MAX);
        ipts.push((
            num,
            Entry {
                label: name_str.into_owned(),
                path,
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
}

impl Default for App {
    fn default() -> Self {
        Self {
            instrument: Instrument::Venus,
            loaded_for: None,
            entries: Vec::new(),
            selected: None,
            error: None,
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
        if self.loaded_for != Some(self.instrument) {
            self.refresh();
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.heading("Neutron IPTS Browser");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Instrument:");
                let prev = self.instrument;
                egui::ComboBox::from_id_source("instrument")
                    .selected_text(self.instrument.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.instrument, Instrument::Venus, "VENUS");
                        ui.selectable_value(&mut self.instrument, Instrument::Mars, "MARS");
                    });
                if prev != self.instrument {
                    self.refresh();
                }
            });
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            ui.label(format!(
                "Accessible IPTS folders under {}: {}",
                self.instrument.root().display(),
                self.ipts_count()
            ));
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            match self.selected.and_then(|i| self.entries.get(i)) {
                Some(entry) => {
                    ui.label(format!("Selected: {}", entry.path.display()));
                }
                None => {
                    ui.label("No IPTS selected");
                }
            }
            ui.add_enabled_ui(self.selected.is_some(), |ui| {
                let button = egui::Button::new("Launch the imaging notebooks");
                if ui
                    .add_sized([ui.available_width(), 28.0], button)
                    .clicked()
                {
                    // TODO: wire up action for the selected entry
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (idx, entry) in self.entries.iter().enumerate() {
                    let is_selected = self.selected == Some(idx);
                    if ui.selectable_label(is_selected, &entry.label).clicked() {
                        self.selected = Some(idx);
                    }
                }
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
