use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct Gpu {
    pub util: u32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub temp: u32,
    pub fan: u32,
}

/// One sample from `nvidia-smi`. No NVML binding: the driver updates that far more often.
pub fn sample() -> Option<Gpu> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu,fan.speed",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 5 {
        return None;
    }
    let num = |s: &str| s.parse::<f64>().ok();
    Some(Gpu {
        util: num(parts[0]).unwrap_or(0.0) as u32,
        mem_used_mb: num(parts[1]).unwrap_or(0.0) as u64,
        mem_total_mb: num(parts[2]).unwrap_or(0.0) as u64,
        temp: num(parts[3]).unwrap_or(0.0) as u32,
        fan: num(parts[4]).unwrap_or(0.0) as u32,
    })
}
