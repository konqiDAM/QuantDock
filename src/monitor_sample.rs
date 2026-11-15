//! Field sampling for the overlay: one background thread, per-field history.

use crate::monitor_config::{field_def, Config};
use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Num(f64),
    Text(String),
    /// Memory pairs, both in MB: used, total.
    UsedTotal(f64, f64),
}

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub fields: BTreeMap<&'static str, Value>,
    /// User-picked sensors, keyed `lhm:<SensorId>`.
    pub custom: BTreeMap<String, crate::monitor_lhm::SensorValue>,
    /// Recent values per field, oldest first, used by graph styles.
    pub history: BTreeMap<&'static str, Vec<f64>>,
}

#[derive(Debug, Clone)]
pub struct History {
    pub cap: usize,
    pub items: Vec<f64>,
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            items: Vec::new(),
        }
    }

    pub fn push(&mut self, value: f64) {
        self.items.push(value);
        if self.items.len() > self.cap {
            let excess = self.items.len() - self.cap;
            self.items.drain(..excess);
        }
    }

    pub fn as_slice(&self) -> &[f64] {
        &self.items
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Cores {
    pub total: f32,
    pub peak: f32,
    pub hot: usize,
}

/// Numeric series kept for graphs. Memory pairs contribute used GB so their
/// sparkline is comparable with a GB threshold.
pub fn series_of(value: &Value) -> Option<f64> {
    match value {
        Value::Num(v) => Some(*v),
        Value::UsedTotal(used, _) => Some(*used / 1024.0),
        Value::Text(_) => None,
    }
}

pub fn aggregate_cores(cores: &[f32], hot_at: f32) -> Cores {
    if cores.is_empty() {
        return Cores {
            total: 0.0,
            peak: 0.0,
            hot: 0,
        };
    }
    let total = cores.iter().sum::<f32>() / cores.len() as f32;
    let peak = cores.iter().cloned().fold(0.0f32, f32::max);
    let hot = cores.iter().filter(|c| **c >= hot_at).count();
    Cores { total, peak, hot }
}

#[derive(Debug, Clone)]
pub struct GpuSample {
    pub util: f32,
    pub mem_util: Option<f32>,
    pub mem_used_mb: f32,
    pub mem_total_mb: f32,
    pub temp: Option<f32>,
    pub fan: Option<f32>,
    pub power: Option<f32>,
    pub power_limit: Option<f32>,
    pub clock: Option<f32>,
    pub pstate: String,
}

/// Parses one `nvidia-smi --format=csv,noheader,nounits` line. `[N/A]` becomes `None`.
pub fn parse_nvidia_smi(line: &str) -> Option<GpuSample> {
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 10 {
        return None;
    }
    let num = |s: &str| s.parse::<f32>().ok();
    Some(GpuSample {
        util: num(parts[0])?,
        mem_util: num(parts[1]),
        mem_used_mb: num(parts[2])?,
        mem_total_mb: num(parts[3])?,
        temp: num(parts[4]),
        fan: num(parts[5]),
        power: num(parts[6]),
        power_limit: num(parts[7]),
        clock: num(parts[8]),
        pstate: parts[9].to_owned(),
    })
}

const GPU_FIELDS: &[&str] = &[
    "gpu", "vram", "vram_pct", "vram_bus", "gpu_temp", "fan", "power", "clock", "pstate",
];
/// Fields that only LibreHardwareMonitor can supply.
const LHM_FIELDS: &[&str] = &[
    "cpu_temp", "cpu_power", "board_temp", "gpu_hotspot", "gpu_mem_temp", "gpu_shared", "fan_rpm",
];
const PROCESS_FIELDS: &[&str] = &["llama_ram", "uptime"];
const PROCESS_REFRESH: Duration = Duration::from_secs(5);
const DISK_FIELDS: &[&str] = &["disk_free"];
const SERVER_FIELDS: &[&str] = &["model", "alias", "ctx"];

fn refresh_due(last: Instant, now: Instant, every: Duration) -> bool {
    now.duration_since(last) >= every
}

fn should_refresh(first: bool, last: Instant, now: Instant, every: Duration) -> bool {
    first || refresh_due(last, now, every)
}

fn shows(cfg: &Config, wanted: &[&str]) -> bool {
    cfg.monitor
        .rows
        .iter()
        .flat_map(|r| r.fields.iter())
        .any(|id| wanted.contains(&id.as_str()))
}

pub fn needs_gpu(cfg: &Config) -> bool {
    shows(cfg, GPU_FIELDS)
}

/// True when a visible field needs the optional hardware-sensor backend.
pub fn needs_lhm(cfg: &Config) -> bool {
    cfg.sensors.enabled && shows(cfg, LHM_FIELDS)
}

/// Starts the one sampling thread. It exits when the receiver is dropped.
pub fn start_sampler(cfg: Arc<RwLock<Config>>) -> Receiver<Sample> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut lhm_reader = crate::monitor_lhm::Reader::new();
        let mut last_lhm =
            std::time::Instant::now() - std::time::Duration::from_secs(60);
        let Ok(mut sys) = sysinfo::System::new() else {
            return;
        };
        let mut history: BTreeMap<&'static str, History> = BTreeMap::new();
        let mut last_processes = Instant::now();
        let mut refresh_processes_now = true;

        loop {
            let Ok(snapshot) = cfg.read().map(|c| c.clone()) else {
                break;
            };
            let cap = snapshot.monitor.graph_samples;

            let mut sample = Sample::default();

            if needs_gpu(&snapshot) {
                if let Some(line) = crate::gpu::raw_query() {
                    if let Some(g) = parse_nvidia_smi(&line) {
                        sample.fields.insert("gpu", Value::Num(g.util as f64));
                        sample
                            .fields
                            .insert("vram", Value::UsedTotal(g.mem_used_mb as f64, g.mem_total_mb as f64));
                        if g.mem_total_mb > 0.0 {
                            sample.fields.insert(
                                "vram_pct",
                                Value::Num((g.mem_used_mb / g.mem_total_mb * 100.0) as f64),
                            );
                        }
                        if let Some(m) = g.mem_util {
                            sample.fields.insert("vram_bus", Value::Num(m as f64));
                        }
                        if let Some(t) = g.temp {
                            sample.fields.insert("gpu_temp", Value::Num(t as f64));
                        }
                        if let Some(f) = g.fan {
                            sample.fields.insert("fan", Value::Num(f as f64));
                        }
                        if let Some(p) = g.power {
                            sample.fields.insert("power", Value::Num(p as f64));
                        }
                        if let Some(c) = g.clock {
                            sample.fields.insert("clock", Value::Num(c as f64));
                        }
                        sample.fields.insert("pstate", Value::Text(g.pstate));
                        let _ = g.power_limit;
                    }
                }
            }

            if needs_lhm(&snapshot)
                && last_lhm.elapsed()
                    >= std::time::Duration::from_millis(snapshot.sensors.interval_ms)
            {
                last_lhm = std::time::Instant::now();
                if let Some(data) = lhm_reader.read() {
                    let lhm = data.named.clone();
                    // Only the sensors the user placed; the map keeps its capacity.
                    sample.custom.clear();
                    for id in snapshot.monitor.custom.keys() {
                        if let Some(v) = data.all.get(id) {
                            sample.custom.insert(id.clone(), v.clone());
                        }
                    }
                    let mut put = |id: &'static str, v: Option<f64>| {
                        if let Some(v) = v {
                            sample.fields.insert(id, Value::Num(v));
                        }
                    };
                    put("cpu_temp", lhm.cpu_temp);
                    put("cpu_power", lhm.cpu_power);
                    put("board_temp", lhm.board_temp);
                    put("gpu_hotspot", lhm.gpu_hotspot);
                    put("gpu_mem_temp", lhm.gpu_mem_temp);
                    put("gpu_shared", lhm.gpu_shared_mb);
                    put("fan_rpm", lhm.fan_rpm);
                }
            }

            sys.refresh_cpu_usage();
            let cores: Vec<f32> = sys.cpus().iter().map(|c| c.usage()).collect();
            let hot_at = snapshot
                .thresholds
                .get("cpu_peak")
                .and_then(|t| t.warn)
                .unwrap_or(80.0) as f32;
            let agg = aggregate_cores(&cores, hot_at);
            sample.fields.insert("cpu", Value::Num(agg.total as f64));
            sample.fields.insert("cpu_peak", Value::Num(agg.peak as f64));
            sample.fields.insert("cpu_hot", Value::Num(agg.hot as f64));

            const MB: f64 = 1024.0 * 1024.0;
            if let Some(mem) = crate::sysmem::read() {
                sample
                    .fields
                    .insert("ram", Value::UsedTotal(mem.used_mb, mem.total_mb));
                if mem.total_mb > 0.0 {
                    sample
                        .fields
                        .insert("ram_pct", Value::Num(mem.used_mb / mem.total_mb * 100.0));
                }
                if mem.swap_total_mb > 0.0 {
                    sample.fields.insert(
                        "swap",
                        Value::UsedTotal(mem.swap_used_mb, mem.swap_total_mb),
                    );
                }
            }

            if shows(&snapshot, DISK_FIELDS) {
                let root = crate::model::models_root();
                if let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() {
                    let best = disks
                        .iter()
                        .filter(|d| root.starts_with(d.mount_point()))
                        .max_by_key(|d| d.mount_point().as_os_str().len());
                    if let Some(d) = best {
                        sample.fields.insert(
                            "disk_free",
                            Value::Num(d.available_space() as f64 / MB),
                        );
                    }
                }
            }

            if shows(&snapshot, SERVER_FIELDS) {
                if let Some(st) = crate::server::status() {
                    sample.fields.insert("alias", Value::Text(st.served_id.clone()));
                    if let Some(path) = &st.model_path {
                        if let Some(name) = std::path::Path::new(path)
                            .parent()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().to_string())
                        {
                            sample.fields.insert("model", Value::Text(name));
                        }
                    }
                    if let Some(ctx) = st.ctx {
                        let text = if ctx >= 1000 {
                            format!("{}K", ctx / 1000)
                        } else {
                            ctx.to_string()
                        };
                        sample.fields.insert("ctx", Value::Text(text));
                    }
                }
            }

            if shows(&snapshot, PROCESS_FIELDS) {
                let now = Instant::now();
                if should_refresh(refresh_processes_now, last_processes, now, PROCESS_REFRESH) {
                    refresh_processes_now = false;
                    last_processes = now;
                    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                }
                let llama = sys
                    .processes()
                    .values()
                    .find(|p| p.name().to_string_lossy().contains("llama-server"));
                if let Some(p) = llama {
                    sample
                        .fields
                        .insert("llama_ram", Value::Num(p.memory() as f64 / MB));
                    let secs = p.run_time();
                    sample.fields.insert(
                        "uptime",
                        Value::Text(format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)),
                    );
                }
            }

            sample.fields.insert("port", Value::Text(crate::model::PORT.to_string()));

            for (id, value) in &sample.fields {
                let Some(v) = series_of(value) else { continue };
                if field_def(id).is_none() {
                    continue;
                }
                let h = history.entry(id).or_insert_with(|| History::new(cap));
                h.cap = cap;
                h.push(v);
            }
            for (id, h) in &history {
                sample.history.insert(id, h.as_slice().to_vec());
            }

            if tx.send(sample).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(snapshot.monitor.interval_ms));
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor_config::{Config, Row};

    #[test]
    fn parses_full_nvidia_smi_line() {
        let g = parse_nvidia_smi("5, 1, 3460, 24576, 52, 61, 112.15, 420.00, 225, P0").unwrap();
        assert_eq!(g.util, 5.0);
        assert_eq!(g.mem_used_mb, 3460.0);
        assert_eq!(g.temp, Some(52.0));
        assert_eq!(g.pstate, "P0");
    }

    #[test]
    fn parses_na_fields_as_none() {
        let g = parse_nvidia_smi("0, 0, 100, 24576, 40, [N/A], [N/A], 420.00, 210, P8").unwrap();
        assert_eq!(g.fan, None);
        assert_eq!(g.power, None);
    }

    #[test]
    fn malformed_line_is_none() {
        assert!(parse_nvidia_smi("garbage").is_none());
    }

    #[test]
    fn aggregates_cores() {
        let c = aggregate_cores(&[10.0, 95.0, 82.0, 3.0], 80.0);
        assert_eq!(c.peak, 95.0);
        assert_eq!(c.hot, 2);
        assert!((c.total - 47.5).abs() < 0.01);
    }

    #[test]
    fn history_respects_capacity() {
        let mut h = History::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.push(v);
        }
        assert_eq!(h.as_slice(), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn series_includes_memory_pairs_in_gb() {
        assert_eq!(series_of(&Value::Num(15.0)), Some(15.0));
        assert_eq!(
            series_of(&Value::UsedTotal(3.0 * 1024.0, 24.0 * 1024.0)),
            Some(3.0)
        );
        assert_eq!(series_of(&Value::Text("x".into())), None);
    }

    #[test]
    fn gpu_is_skipped_when_no_gpu_field_is_shown() {
        let mut c = Config::defaults();
        c.monitor.rows = vec![Row {
            fields: vec!["cpu".into()],
        }];
        assert!(!needs_gpu(&c));
        c.monitor.rows[0].fields.push("gpu".into());
        assert!(needs_gpu(&c));
    }

    #[test]
    #[ignore = "spawns nvidia-smi and waits for a real sample"]
    fn sampler_produces_a_sample() {
        let cfg = Arc::new(RwLock::new(Config::defaults()));
        let rx = start_sampler(cfg);
        let s = rx
            .recv_timeout(Duration::from_secs(8))
            .expect("no sample within 8s");
        assert!(matches!(s.fields.get("cpu"), Some(Value::Num(_))));
        assert!(matches!(s.fields.get("ram"), Some(Value::UsedTotal(_, _))));
        assert!(
            matches!(s.fields.get("gpu"), Some(Value::Num(_))),
            "nvidia-smi should work on this machine: {:?}",
            s.fields
        );
        println!("{:?}", s.fields);
    }

    #[test]
    fn process_refresh_is_due_after_five_seconds() {
        let now = Instant::now();
        assert!(refresh_due(now - Duration::from_secs(5), now, PROCESS_REFRESH));
        assert!(!refresh_due(now - Duration::from_secs(4), now, PROCESS_REFRESH));
    }

    #[test]
    fn first_process_refresh_is_immediate() {
        let now = Instant::now();
        assert!(should_refresh(true, now, now, PROCESS_REFRESH));
        assert!(!should_refresh(false, now, now, PROCESS_REFRESH));
    }
}
