pub mod gpu;
pub mod hf;
pub mod model;
pub mod server;

/// Run a child process without flashing a console window.
/// Rust's std does not set CREATE_NO_WINDOW when stdio is piped, so a console
/// app like nvidia-smi pops up a black window for every call.
pub fn hidden(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Writes a panic and a backtrace to `%APPDATA%\QuantDock\crash.log`, then runs the
/// default hook. GUI builds have no console, so this is the only crash evidence.
pub fn install_panic_log() {
    let path = monitor_config::data_dir().join("crash.log");
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(
                file,
                "\n--- panic ---\n{info}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
        default(info);
    }));
}
pub mod monitor_config;
pub mod monitor_sample;
pub mod monitor_panel;
pub mod monitor_lhm;
pub mod sysmem;