//! egui-Oberfläche: Quelle wählen, Ziele verwalten, Pegel und Status beobachten.

use crate::config::{Config, TargetConfig};
use crate::devices::{self, DeviceInfo};
use crate::engine::Engine;
use crate::params::{level_to_meter, TargetStatus};
use anyhow::{anyhow, Result};
use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEVICE_REFRESH: Duration = Duration::from_secs(3);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const DEFAULT_SOURCE_LABEL: &str = "Windows-Standardausgabe";

pub fn run_gui() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("shared-audio")
            .with_inner_size([580.0, 640.0])
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "shared-audio",
        options,
        Box::new(|_cc| Ok(Box::new(SharedAudioApp::new()))),
    )
    .map_err(|e| anyhow!("GUI-Fehler: {e}"))
}

struct SharedAudioApp {
    engine: Engine,
    config_path: PathBuf,
    saved: Config,
    last_change: Instant,
    devices: Vec<DeviceInfo>,
    last_refresh: Instant,
    add_choice: Option<String>,
    message: Option<String>,
    /// Angezeigte Pegel (mit Abklingen) je Ziel-ID.
    meters: HashMap<u64, (f32, f32)>,
    /// Anzeigename der gewählten Quelle (für getrennte Geräte).
    source_name: Option<String>,
}

impl SharedAudioApp {
    fn new() -> Self {
        let config_path = Config::default_path();
        let (config, mut message) = match Config::load(&config_path) {
            Ok(c) => (c, None),
            Err(e) => (
                Config::default(),
                Some(format!("Konfiguration ignoriert: {e:#}")),
            ),
        };
        let mut engine = Engine::new(config.source_id.clone());
        for t in &config.targets {
            if let Err(e) = engine.add_target(&t.device_id, &t.name, t.volume, t.muted, t.delay_ms)
            {
                message = Some(format!("{e:#}"));
            }
        }
        Self {
            engine,
            config_path,
            saved: config.clone(),
            last_change: Instant::now(),
            devices: devices::list_render_devices().unwrap_or_default(),
            last_refresh: Instant::now(),
            add_choice: None,
            message,
            meters: HashMap::new(),
            source_name: config.source_name,
        }
    }

    fn current_config(&self) -> Config {
        Config {
            source_id: self.engine.source_id().map(str::to_owned),
            source_name: self.source_name.clone(),
            targets: self
                .engine
                .targets()
                .iter()
                .map(|t| TargetConfig {
                    device_id: t.device_id.clone(),
                    name: t.name.clone(),
                    volume: t.params.volume(),
                    muted: t.params.muted(),
                    delay_ms: t.params.delay_ms(),
                })
                .collect(),
        }
    }

    fn save_if_changed(&mut self, force: bool) {
        let cfg = self.current_config();
        if cfg == self.saved {
            return;
        }
        if force || self.last_change.elapsed() >= SAVE_DEBOUNCE {
            match cfg.save(&self.config_path) {
                Ok(()) => self.saved = cfg,
                Err(e) => self.message = Some(format!("Speichern fehlgeschlagen: {e:#}")),
            }
            self.last_change = Instant::now();
        }
    }

    /// ID der effektiv genutzten Quelle (bei „Standard“ die aktuelle Standardausgabe).
    fn effective_source_id(&self) -> Option<String> {
        match self.engine.source_id() {
            Some(id) => Some(id.to_owned()),
            None => self
                .devices
                .iter()
                .find(|d| d.is_default)
                .map(|d| d.id.clone()),
        }
    }

    fn source_label(&self) -> String {
        match self.engine.source_id() {
            None => DEFAULT_SOURCE_LABEL.to_owned(),
            Some(id) => self
                .devices
                .iter()
                .find(|d| d.id == id)
                .map(|d| d.name.clone())
                .or_else(|| self.source_name.clone())
                .unwrap_or_else(|| id.to_owned()),
        }
    }
}

impl Drop for SharedAudioApp {
    fn drop(&mut self) {
        self.save_if_changed(true);
    }
}

fn status_color(s: TargetStatus) -> egui::Color32 {
    match s {
        TargetStatus::Active => egui::Color32::from_rgb(70, 180, 90),
        TargetStatus::Connecting => egui::Color32::from_rgb(200, 170, 60),
        TargetStatus::Unavailable | TargetStatus::Feedback => egui::Color32::from_rgb(210, 80, 70),
        TargetStatus::Stopped => egui::Color32::GRAY,
    }
}

fn meter_bar(ui: &mut egui::Ui, label: &str, value: f32) {
    ui.horizontal(|ui| {
        ui.add_sized([14.0, 14.0], egui::Label::new(label));
        let color = if value > 0.97 {
            egui::Color32::from_rgb(220, 70, 60)
        } else {
            egui::Color32::from_rgb(70, 180, 90)
        };
        ui.add(
            egui::ProgressBar::new(value)
                .desired_width(ui.available_width())
                .desired_height(10.0)
                .fill(color),
        );
    });
}

impl eframe::App for SharedAudioApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.last_refresh.elapsed() >= DEVICE_REFRESH {
            if let Ok(d) = devices::list_render_devices() {
                self.devices = d;
            }
            self.last_refresh = Instant::now();
        }

        // Pegel einsammeln und abklingen lassen.
        for t in self.engine.targets() {
            let (l, r) = t.params.take_peak();
            let m = self.meters.entry(t.id).or_insert((0.0, 0.0));
            m.0 = level_to_meter(l).max(m.0 * 0.88);
            m.1 = level_to_meter(r).max(m.1 * 0.88);
        }

        let mut remove: Option<u64> = None;
        let mut new_source: Option<(Option<String>, Option<String>)> = None;
        let mut add: Option<DeviceInfo> = None;

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("shared-audio");
            ui.add_space(4.0);

            // ── Quelle ──
            let target_ids: Vec<&str> = self
                .engine
                .targets()
                .iter()
                .map(|t| t.device_id.as_str())
                .collect();
            ui.horizontal(|ui| {
                ui.label("Quelle:");
                egui::ComboBox::from_id_salt("source")
                    .width(320.0)
                    .selected_text(self.source_label())
                    .show_ui(ui, |ui| {
                        let is_default = self.engine.source_id().is_none();
                        if ui
                            .selectable_label(is_default, DEFAULT_SOURCE_LABEL)
                            .clicked()
                        {
                            new_source = Some((None, None));
                        }
                        for d in &self.devices {
                            // Geräte, die bereits Ziel sind, sind als Quelle nicht wählbar.
                            let blocked = target_ids.contains(&d.id.as_str());
                            let selected = self.engine.source_id() == Some(d.id.as_str());
                            let text = if blocked {
                                format!("{} (ist Ziel)", d.name)
                            } else {
                                d.name.clone()
                            };
                            if ui
                                .add_enabled(!blocked, egui::Button::selectable(selected, text))
                                .clicked()
                            {
                                new_source = Some((Some(d.id.clone()), Some(d.name.clone())));
                            }
                        }
                    });
            });
            let src = self.engine.source();
            ui.horizontal(|ui| {
                ui.colored_label(status_color(src.status()), "●");
                let detail = if src.rate() > 0 {
                    format!("{} – {} Hz", src.status().label(), src.rate())
                } else {
                    src.status().label().to_owned()
                };
                ui.label(detail);
            });
            ui.separator();

            // ── Ziel hinzufügen ──
            let source_id = self.effective_source_id();
            let candidates: Vec<&DeviceInfo> = self
                .devices
                .iter()
                .filter(|d| {
                    Some(&d.id) != source_id.as_ref() && !target_ids.contains(&d.id.as_str())
                })
                .collect();
            ui.horizontal(|ui| {
                ui.label("Ziel hinzufügen:");
                let selected_text = self
                    .add_choice
                    .as_ref()
                    .and_then(|id| candidates.iter().find(|d| &d.id == id))
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| "Gerät wählen …".to_owned());
                egui::ComboBox::from_id_salt("add")
                    .width(260.0)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for d in &candidates {
                            let sel = self.add_choice.as_ref() == Some(&d.id);
                            if ui.selectable_label(sel, &d.name).clicked() {
                                self.add_choice = Some(d.id.clone());
                            }
                        }
                    });
                let chosen = self
                    .add_choice
                    .as_ref()
                    .and_then(|id| candidates.iter().find(|d| &d.id == id));
                if ui
                    .add_enabled(chosen.is_some(), egui::Button::new("Hinzufügen"))
                    .clicked()
                {
                    add = chosen.map(|d| (*d).clone());
                }
            });
            if let Some(m) = &self.message {
                ui.colored_label(egui::Color32::from_rgb(210, 80, 70), m);
            }
            ui.separator();

            // ── Ziele ──
            if self.engine.targets().is_empty() {
                ui.label(
                    "Noch keine Ziele. Wähle oben ein Ausgabegerät und klicke auf „Hinzufügen“.",
                );
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for t in self.engine.targets() {
                    let p = &t.params;
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.strong(&t.name);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Entfernen").clicked() {
                                        remove = Some(t.id);
                                    }
                                    let st = p.status();
                                    ui.colored_label(status_color(st), st.label());
                                    ui.colored_label(status_color(st), "●");
                                },
                            );
                        });

                        let mut vol = p.volume() * 100.0;
                        let mut muted = p.muted();
                        ui.horizontal(|ui| {
                            ui.label("Lautstärke");
                            if ui
                                .add(
                                    egui::Slider::new(&mut vol, 0.0..=100.0)
                                        .suffix(" %")
                                        .integer(),
                                )
                                .changed()
                            {
                                p.set_volume(vol / 100.0);
                            }
                            if ui.toggle_value(&mut muted, "Mute").changed() {
                                p.set_muted(muted);
                            }
                        });
                        let mut delay = p.delay_ms() as f32;
                        ui.horizontal(|ui| {
                            ui.label("Verzögerung");
                            if ui
                                .add(
                                    egui::Slider::new(
                                        &mut delay,
                                        0.0..=crate::delay::MAX_DELAY_MS as f32,
                                    )
                                    .suffix(" ms")
                                    .integer(),
                                )
                                .changed()
                            {
                                p.set_delay_ms(delay as u32);
                            }
                        });
                        let (l, r) = self.meters.get(&t.id).copied().unwrap_or((0.0, 0.0));
                        meter_bar(ui, "L", l);
                        meter_bar(ui, "R", r);
                        let (u, o, d) = (
                            p.underruns.load(std::sync::atomic::Ordering::Relaxed),
                            p.overruns.load(std::sync::atomic::Ordering::Relaxed),
                            p.dropped_frames.load(std::sync::atomic::Ordering::Relaxed),
                        );
                        if u + o + d > 0 {
                            ui.small(format!(
                                "Aussetzer: {u} · Überläufe: {o} · verworfene Frames: {d}"
                            ));
                        }
                    });
                    ui.add_space(4.0);
                }
            });
        });

        // Aktionen nach dem Zeichnen ausführen (Engine-Änderungen während der Iteration vermeiden).
        if let Some((id, name)) = new_source {
            match self.engine.set_source(id) {
                Ok(()) => {
                    self.source_name = name;
                    self.message = None;
                }
                Err(e) => self.message = Some(format!("{e:#}")),
            }
        }
        if let Some(d) = add {
            match self.engine.add_target(&d.id, &d.name, 1.0, false, 0) {
                Ok(_) => {
                    self.message = None;
                    self.add_choice = None;
                }
                Err(e) => self.message = Some(format!("{e:#}")),
            }
        }
        if let Some(id) = remove {
            self.engine.remove_target(id);
            self.meters.remove(&id);
        }

        self.save_if_changed(false);
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
