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
    last_models: Instant,
    positioned: bool,
    hwnd: Option<isize>,
}

impl Monitor {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let root = model::models_root();
        Self {
            gpu: None,
            status: None,
            models: model::discover(&root),
            last: Instant::now() - Duration::from_secs(5),
            last_models: Instant::now(),
            positioned: false,
            hwnd: window_handle(cc),
        }
    }

    /// Park the overlay in the top-right corner of the primary monitor.
    fn position_once(&mut self) {
        if self.positioned {
            return;
        }
        self.positioned = true;
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            place_top_right(hwnd);
        }
    }
}

#[cfg(windows)]
fn window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
fn window_handle(_cc: &eframe::CreationContext<'_>) -> Option<isize> {
    None
}

/// Position in physical pixels via the HWND. Going through egui's logical-point
/// viewport command mixes up DPI on scaled displays and parks the window off-screen.
#[cfg(windows)]
fn place_top_right(hwnd: isize) {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos,
    };
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        let scale = (GetDpiForWindow(hwnd).max(96) as f32) / 96.0;
        let margin = (MARGIN * scale).round() as i32;
        let x = info.rcWork.right - (rect.right - rect.left) - margin;
        let y = info.rcWork.top + margin;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

impl eframe::App for Monitor {
    /// Fully transparent so only the rounded panel we paint is visible.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.position_once();

        if self.last.elapsed() >= Duration::from_millis(1000) {
            self.last = Instant::now();
            self.gpu = gpu::sample();
            self.status = server::status();
        }
        // Re-read the model folders rarely: this is display metadata, not live data.
        if self.last_models.elapsed() >= Duration::from_secs(10) {
            self.last_models = Instant::now();
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
                            egui::RichText::new("drag to move  /  x or right-click to close")
                                .small()
                                .weak(),
                        );
                    }
                    None => {
                        ui.label(egui::RichText::new("nvidia-smi unavailable").weak());
                    }
                }
            });

        // Drag anywhere to move the overlay.
        let rect = panel.response.rect;
        let drag = ui.interact(
            rect,
            ui.scope_id().with("drag"),
            egui::Sense::click_and_drag(),
        );
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        // There is no title bar, so put an explicit close button in the corner.
        // It is placed after the drag area, and the drag area also hit-tests the
        // click position, so either path closes the overlay.
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 28.0, rect.top() + 7.0),
            egui::vec2(20.0, 20.0),
        );
        let close = ui.put(
            close_rect,
            egui::Button::new(egui::RichText::new("x").size(14.0)).frame(false),
        );

        let clicked_close = drag.clicked()
            && ctx
                .pointer_interact_pos()
                .is_some_and(|p| close_rect.contains(p));
        if clicked_close || close.clicked() || drag.secondary_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Only one overlay may exist. A named mutex is released by the OS when the owning
/// process dies, so a stale window can never block a new one.
#[cfg(windows)]
fn already_running() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        let handle = CreateMutexW(None, false, windows::core::w!("Local\\QuantDockMonitor"));
        let err = GetLastError();
        // Hold the handle for the process lifetime; dropping it would release the name.
        std::mem::forget(handle);
        err == ERROR_ALREADY_EXISTS
    }
}

#[cfg(not(windows))]
fn already_running() -> bool {
    false
}

fn main() -> eframe::Result {
    if already_running() {
        return Ok(());
    }
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
        Box::new(|cc| Ok(Box::new(Monitor::new(cc)))),
    )
}
