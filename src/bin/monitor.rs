// No console window for the released binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use quantdock::{gpu, model, server};
use std::time::{Duration, Instant};

const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 238.0;
const MARGIN: f32 = 16.0;

struct Monitor {
    gpu: Option<gpu::Gpu>,
    status: Option<server::Status>,
    models: Vec<model::Model>,
    last: Instant,
    positioned: bool,
}

impl Monitor {
    fn new() -> Self {
        let root = model::models_root();
        Self {
            gpu: None,
            status: None,
            models: model::discover(&root),
            last: Instant::now() - Duration::from_secs(5),
            positioned: false,
        }
    }

    /// Park the overlay in the top-right corner of the primary monitor.
    fn position_once(&mut self, ctx: &egui::Context) {
        if self.positioned {
            return;
        }
        self.positioned = true;
        let Some((_left, top, right, _bottom, scale)) = primary_monitor() else {
            return;
        };
        // GetMonitorInfoW gives physical pixels; egui positions in logical points.
        let x = right as f32 / scale - WIDTH - MARGIN;
        let y = top as f32 / scale + MARGIN;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
    }
}

/// Primary monitor: work area (left, top, right, bottom) in physical pixels, plus its
/// scale factor. egui reports pixels_per_point too late to place the window on frame 1,
/// so ask Windows for the real DPI instead.
#[cfg(windows)]
fn primary_monitor() -> Option<(i32, i32, i32, i32, f32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let h = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(h, &mut info).as_bool() {
            return None;
        }
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = (dpi_x.max(96) as f32) / 96.0;
        let r = info.rcWork;
        Some((r.left, r.top, r.right, r.bottom, scale))
    }
}

#[cfg(not(windows))]
fn primary_monitor() -> Option<(i32, i32, i32, i32, f32)> {
    None
}

impl eframe::App for Monitor {
    /// Fully transparent so only the rounded panel we paint is visible.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.position_once(&ctx);

        if self.last.elapsed() >= Duration::from_millis(1000) {
            self.last = Instant::now();
            self.gpu = gpu::sample();
            self.status = server::status();
            self.models = model::discover(&model::models_root());
        }
        ctx.request_repaint_after(Duration::from_millis(400));

        let panel = egui::Frame::group(ui.style())
            .fill(egui::Color32::from_black_alpha(175))
            .corner_radius(12.0)
            .stroke(egui::Stroke::new(
                1.0,
                egui::Color32::from_white_alpha(28),
            ))
            .shadow(egui::Shadow::NONE)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 5.0;

                // Model line
                match self.status.as_ref().and_then(|st| model::loaded(&self.models, st)) {
                    Some(m) => {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&m.cfg.name).strong().size(13.0),
                            );
                        });
                        ui.label(
                            egui::RichText::new(format!("[{}]", m.cfg.alias))
                                .small()
                                .color(egui::Color32::from_rgb(120, 190, 255)),
                        );
                    }
                    None => {
                        let txt = match &self.status {
                            Some(st) => format!("serving {}", st.served_id),
                            None => "no model loaded".to_owned(),
                        };
                        ui.label(egui::RichText::new(txt).weak().size(13.0));
                    }
                }

                ui.separator();

                match &self.gpu {
                    Some(g) => {
                        ui.add(
                            egui::ProgressBar::new(g.util as f32 / 100.0)
                                .desired_width(f32::INFINITY)
                                .text(format!("GPU  {}%", g.util)),
                        );
                        let frac = g.mem_used_mb as f32 / g.mem_total_mb.max(1) as f32;
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(f32::INFINITY)
                                .text(format!(
                                    "VRAM {:.1} / {:.1} GB",
                                    g.mem_used_mb as f64 / 1024.0,
                                    g.mem_total_mb as f64 / 1024.0
                                )),
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{} °C", g.temp)).small());
                            ui.separator();
                            ui.label(egui::RichText::new(format!("fan {}%", g.fan)).small());
                        });
                        ui.label(
                            egui::RichText::new("drag to move  /  right-click to quit")
                                .small()
                                .weak(),
                        );
                    }
                    None => {
                        ui.label(egui::RichText::new("nvidia-smi unavailable").weak());
                    }
                }
            });

        // Drag anywhere to move the overlay; right-click to close it.
        let rect = panel.response.rect;
        let drag = ui.interact(rect, ui.scope_id().with("drag"), egui::Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.secondary_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("QuantDock Monitor")
            .with_inner_size([WIDTH, HEIGHT])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "QuantDock Monitor",
        options,
        Box::new(|_cc| Ok(Box::new(Monitor::new()))),
    )
}
