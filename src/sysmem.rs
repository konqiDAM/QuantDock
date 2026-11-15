//! One `GlobalMemoryStatusEx` call into a stack struct. sysinfo's memory refresh
//! allocated through `K32GetPerformanceInfo` on every tick; this does not allocate.

#[derive(Debug, Clone, Copy, Default)]
pub struct Mem {
    pub used_mb: f64,
    pub total_mb: f64,
    pub swap_used_mb: f64,
    pub swap_total_mb: f64,
}

#[cfg(windows)]
pub fn read() -> Option<Mem> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut s = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe {
        GlobalMemoryStatusEx(&mut s).ok()?;
    }
    Some(from_status(&s))
}

#[cfg(not(windows))]
pub fn read() -> Option<Mem> {
    None
}

fn from_status(s: &windows::Win32::System::SystemInformation::MEMORYSTATUSEX) -> Mem {
    const MB: f64 = 1024.0 * 1024.0;

    let total_phys = s.ullTotalPhys;
    let avail_phys = s.ullAvailPhys.min(total_phys);
    let total_page = s.ullTotalPageFile;
    let avail_page = s.ullAvailPageFile.min(total_page);

    let used = total_phys.saturating_sub(avail_phys);
    let swap_total = total_page.saturating_sub(total_phys);
    // sysinfo reports page-file usage, not commit above used RAM.
    let commit_total = total_page.saturating_sub(avail_page);
    let swap_used = commit_total.saturating_sub(total_phys);

    Mem {
        used_mb: used as f64 / MB,
        total_mb: total_phys as f64 / MB,
        swap_used_mb: swap_used as f64 / MB,
        swap_total_mb: swap_total as f64 / MB,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::System::SystemInformation::MEMORYSTATUSEX;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn computes_used_and_swap_from_memory_status() {
        let s = MEMORYSTATUSEX {
            ullTotalPhys: 32 * GB,
            ullAvailPhys: 8 * GB,
            ullTotalPageFile: 40 * GB,
            ullAvailPageFile: 4 * GB,
            ..Default::default()
        };
        let m = from_status(&s);
        assert_eq!(m.total_mb, 32.0 * 1024.0);
        assert_eq!(m.used_mb, 24.0 * 1024.0);
        assert_eq!(m.swap_total_mb, 8.0 * 1024.0);
        assert_eq!(m.swap_used_mb, 4.0 * 1024.0);
    }

    #[test]
    fn no_page_file_means_zero_swap() {
        let s = MEMORYSTATUSEX {
            ullTotalPhys: 16 * GB,
            ullAvailPhys: 4 * GB,
            ullTotalPageFile: 16 * GB,
            ullAvailPageFile: 12 * GB,
            ..Default::default()
        };
        let m = from_status(&s);
        assert_eq!(m.swap_total_mb, 0.0);
        assert_eq!(m.swap_used_mb, 0.0);
    }
}
