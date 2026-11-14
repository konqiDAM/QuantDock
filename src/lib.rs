pub mod gpu;
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
pub mod monitor_config;
pub mod monitor_sample;
