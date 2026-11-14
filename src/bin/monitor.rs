// No console window for the released binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use quantdock::monitor_config::{self, Config, Corner, Watcher};
use quantdock::monitor_panel;
use quantdock::monitor_sample::{start_sampler, Sample};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 158.0;

struct Monitor {
    cfg: Arc<RwLock<Config>>,
    watcher: Watcher,
    samples: Receiver<Sample>,
    sample: Sample,
    positioned: bool,
    hwnd: Option<isize>,
    last_style: Instant,
    last_watch: Instant,
    always_on_top: bool,
    /// Position only after the first content-driven resize, otherwise the corner is
    /// computed from the initial 320x158 window and the overlay ends up off-screen.
    frames: u8,
}

impl Monitor {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let path = monitor_config::settings_path();
        let cfg = match Config::load_existing(&path) {
            Ok(Some(c)) => c,
            Ok(None) => {
                let defaults = Config::defaults();
                let _ = defaults.save(&path);
                defaults
            }
            Err(e) => {
                // Never overwrite a file we cannot read: the manager offers a reset.
                eprintln!("quantdock monitor: {e}");
                Config::defaults()
            }
        };
        let always_on_top = cfg.monitor.always_on_top;
        let cfg = Arc::new(RwLock::new(cfg));
        let samples = start_sampler(cfg.clone());
        Self {
            cfg,
            watcher: Watcher::new(path),
            samples,
            sample: Sample::default(),
            positioned: false,
            hwnd: window_handle(cc),
            last_style: Instant::now(),
            last_watch: Instant::now(),
            always_on_top,
            frames: 0,
        }
    }

    fn position_once(&mut self) {
        if self.positioned {
            return;
        }
        self.positioned = true;
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            let cfg = self.cfg.read().map(|c| c.clone()).unwrap_or_default();
            let hwnd = windows::Win32::Foundation::HWND(hwnd as *mut _);
            match monitor_config::load_window_pos() {
                Some(pos) => set_rect(hwnd, pos.x as i32, pos.y as i32, cfg.monitor.always_on_top),
                None => place_corner(
                    hwnd,
                    cfg.monitor.position,
                    cfg.monitor.margin,
                    cfg.monitor.always_on_top,
                ),
            }
        }
    }

    fn apply_topmost(&mut self) {
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            use windows::Win32::Foundation::HWND;
            let _ = set_topmost(HWND(hwnd as *mut _), self.always_on_top);
        }
    }

    fn save_position(&self) {
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            if let Some((x, y)) = window_pos(hwnd) {
                let _ = monitor_config::save_window_pos(egui::pos2(x as f32, y as f32));
            }
        }
    }
}

impl eframe::App for Monitor {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frames = self.frames.saturating_add(1);
        if self.frames >= 2 {
            self.position_once();
        }

        // winit rewrites the extended style during startup; keep the window off the taskbar.
        if self.last_style.elapsed() >= Duration::from_secs(2) {
            self.last_style = Instant::now();
            #[cfg(windows)]
            if let Some(hwnd) = self.hwnd {
                hide_from_taskbar(windows::Win32::Foundation::HWND(hwnd as *mut _));
            }
        }

        if self.last_watch.elapsed() >= Duration::from_secs(1) {
            self.last_watch = Instant::now();
            if self.watcher.changed() {
                if let Ok(fresh) = Config::load(&self.watcher.path) {
                    let changed_top = fresh.monitor.always_on_top != self.always_on_top;
                    self.always_on_top = fresh.monitor.always_on_top;
                    if let Ok(mut guard) = self.cfg.write() {
                        *guard = fresh;
                    }
                    if changed_top {
                        self.apply_topmost();
                    }
                }
            }
        }

        while let Ok(sample) = self.samples.try_recv() {
            self.sample = sample;
        }

        let cfg = self.cfg.read().map(|c| c.clone()).unwrap_or_default();
        let current = ui.max_rect().size();
        let want = monitor_panel::render(ui, &cfg, &self.sample);
        let want = egui::vec2(want.x.max(40.0), want.y.max(32.0));
        let want = match self.hwnd {
            #[cfg(windows)]
            Some(hwnd) => clamp_to_work_area(hwnd, want),
            _ => want,
        };
        if (want.x - current.x).abs() > 1.0 || (want.y - current.y).abs() > 1.0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
        }
        ctx.request_repaint_after(Duration::from_millis(250));

        let rect = ui.max_rect();
        let drag = ui.interact(
            rect,
            ui.scope_id().with("drag"),
            egui::Sense::click_and_drag(),
        );
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.drag_stopped() {
            self.save_position();
        }

        let close_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 34.0, rect.top() + 8.0),
            egui::vec2(24.0, 24.0),
        );
        if cfg.monitor.show_close {
            // Painted, not a widget: `ui.put` at an absolute rect leaves an artifact
            // above the window's top-right corner, and the drag area would win the
            // click anyway.
            let hovered = ctx
                .pointer_hover_pos()
                .is_some_and(|p| close_rect.contains(p));
            ui.painter().rect_filled(
                close_rect,
                5.0,
                if hovered {
                    egui::Color32::from_rgb(150, 60, 60)
                } else {
                    egui::Color32::from_white_alpha(16)
                },
            );
            ui.painter().text(
                close_rect.center(),
                egui::Align2::CENTER_CENTER,
                "x",
                egui::FontId::proportional(cfg.monitor.font_size * 1.1),
                egui::Color32::from_rgb(225, 225, 225),
            );
        }

        let clicked_close = cfg.monitor.show_close
            && drag.clicked()
            && ctx
                .pointer_interact_pos()
                .is_some_and(|p| close_rect.contains(p));
        if clicked_close || drag.secondary_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Keeps an auto-sized panel inside the monitor's work area, in egui points.
#[cfg(windows)]
fn clamp_to_work_area(hwnd: isize, size: egui::Vec2) -> egui::Vec2 {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    unsafe {
        let h = HWND(hwnd as *mut _);
        let scale = (GetDpiForWindow(h).max(96) as f32) / 96.0;
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return size;
        }
        let w = (info.rcWork.right - info.rcWork.left) as f32 / scale;
        let h = (info.rcWork.bottom - info.rcWork.top) as f32 / scale;
        egui::vec2(size.x.min(w - 32.0), size.y.min(h - 32.0))
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

#[cfg(windows)]
fn set_topmost(hwnd: windows::Win32::Foundation::HWND, on: bool) -> windows::core::Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };
    unsafe {
        SetWindowPos(
            hwnd,
            Some(if on { HWND_TOPMOST } else { HWND_NOTOPMOST }),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
}

#[cfg(windows)]
fn set_rect(hwnd: windows::Win32::Foundation::HWND, x: i32, y: i32, topmost: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos,
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST }),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Places the window in one corner of the primary work area, in physical pixels.
#[cfg(windows)]
fn place_corner(
    hwnd: windows::Win32::Foundation::HWND,
    corner: Corner,
    margin: f32,
    topmost: bool,
) {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
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
        let m = (margin * scale).round() as i32;
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let work = info.rcWork;
        let x = match corner {
            Corner::TopLeft | Corner::BottomLeft => work.left + m,
            Corner::TopRight | Corner::BottomRight => work.right - w - m,
        };
        let y = match corner {
            Corner::TopLeft | Corner::TopRight => work.top + m,
            Corner::BottomLeft | Corner::BottomRight => work.bottom - h - m,
        };
        set_rect(hwnd, x, y, topmost);
    }
}

#[cfg(windows)]
fn window_pos(hwnd: isize) -> Option<(i32, i32)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(HWND(hwnd as *mut _), &mut rect).ok()?;
        Some((rect.left, rect.top))
    }
}

#[cfg(windows)]
fn hide_from_taskbar(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex = (ex & !(WS_EX_APPWINDOW.0 as isize)) | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
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
        std::mem::forget(handle);
        err == ERROR_ALREADY_EXISTS
    }
}

#[cfg(not(windows))]
fn already_running() -> bool {
    false
}

/// The overlay hides itself from the taskbar, so a second launch is the way to
/// find it again if it ends up behind something.
#[cfg(windows)]
fn focus_existing() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow,
        SetWindowPos,
    };
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, windows::core::w!("QuantDock Monitor")) {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(windows))]
fn focus_existing() {}

fn main() -> eframe::Result {
    if already_running() {
        focus_existing();
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("QuantDock Monitor")
            .with_inner_size([WIDTH, HEIGHT])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "QuantDock Monitor",
        options,
        Box::new(|cc| Ok(Box::new(Monitor::new(cc)))),
    )
}
