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
pub mod monitor_panel;
pub mod monitor_lhm;
pub mod sysmem;
/// Returns unused pages to the OS. Windows keeps the heap's high-water mark after a
/// burst of allocations, which looks like a leak in Task Manager even when the live
/// heap is flat.
pub fn trim_working_set() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}