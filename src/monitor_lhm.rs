//! LibreHardwareMonitor integration: read its web server JSON.
//!
//! Sensors are matched by `SensorId` / `HardwareId` prefixes
//! (`/intelcpu`, `/amdcpu`, `/nvidiagpu`, `/atigpu`, `/lpc`), never by hardware
//! display names, so the mapping works on any machine.


use std::borrow::Cow;

pub const URL: &str = "http://127.0.0.1:8085/data.json";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LhmSample {
    pub cpu_temp: Option<f64>,
    pub cpu_power: Option<f64>,
    pub board_temp: Option<f64>,
    pub gpu_hotspot: Option<f64>,
    pub gpu_mem_temp: Option<f64>,
    pub gpu_shared_mb: Option<f64>,
    pub fan_rpm: Option<f64>,
}

/// One sensor in the picker's hardware tree. Parsed only when the picker opens.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorItem {
    pub id: String,
    pub text: String,
    pub unit: String,
    pub value: f64,
    pub hardware: Vec<String>,
    pub hardware_id: String,
    pub group: String,
}

/// Any numeric sensor LHM reports, for the user-picked "custom" fields.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorValue {
    pub text: String,
    pub unit: String,
    pub value: f64,
}

#[derive(Debug, Clone, Default)]
pub struct LhmData {
    pub named: LhmSample,
    pub all: std::collections::BTreeMap<String, SensorValue>,
}

/// Mirror of the data.json node shape. Fields borrow when possible; LHM escapes
/// non-ASCII values (`\u00B0`), and those must allocate instead of failing.
#[derive(serde::Deserialize)]
struct Node<'a> {
    #[serde(rename = "Text", default, borrow)]
    text: Cow<'a, str>,
    #[serde(rename = "Value", default, borrow)]
    value: Cow<'a, str>,
    #[serde(rename = "RawValue", default, borrow)]
    raw: Cow<'a, str>,
    #[serde(rename = "SensorId", default)]
    sensor_id: Option<&'a str>,
    #[serde(rename = "HardwareId", default)]
    hardware_id: Option<&'a str>,
    #[serde(rename = "Children", default)]
    children: Vec<Node<'a>>,
}

pub fn parse_data(json: &str) -> Option<LhmData> {
    let root: Node = serde_json::from_str(json).ok()?;
    let mut data = LhmData::default();
    let mut sensors = 0usize;
    walk(&root, &mut data, &mut sensors);
    (sensors > 0).then_some(data)
}

pub fn parse(json: &str) -> Option<LhmSample> {
    parse_data(json).map(|d| d.named)
}

/// One agent for the process: building one per request made a new connection pool
/// and a pile of buffers every second.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_millis(900)))
            .timeout_connect(Some(std::time::Duration::from_millis(250)))
            .build()
            .new_agent()
    })
}

pub fn fetch_data() -> Option<LhmData> {
    let mut resp = agent().get(URL).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    parse_data(&text)
}

/// Liveness probe: one request, named sensors only.
pub fn fetch() -> Option<LhmSample> {
    fetch_data().map(|d| d.named)
}

/// True when a sensor exists only behind LibreHardwareMonitor.
pub fn field_needs_lhm(id: &str) -> bool {
    matches!(
        id,
        "cpu_temp" | "cpu_power" | "board_temp" | "gpu_hotspot" | "gpu_mem_temp" | "gpu_shared"
            | "fan_rpm"
    )
}

fn walk<'a>(node: &'a Node<'a>, out: &mut LhmData, sensors: &mut usize) {
    if let Some(id) = node.sensor_id {
        *sensors += 1;
        let value = node.value.as_ref();
        let raw = if node.raw.is_empty() {
            value
        } else {
            node.raw.as_ref()
        };
        apply(id, node.text.as_ref(), value, raw, &mut out.named);
        if let Some((value, unit)) = reading(node) {
            out.all.insert(
                format!("lhm:{id}"),
                SensorValue {
                    text: node.text.as_ref().to_owned(),
                    unit,
                    value,
                },
            );
        }
    }
    for child in &node.children {
        walk(child, out, sensors);
    }
}

/// The picker's view of the tree: one item per numeric sensor, with the hardware
/// path and group names the flat `walk` drops. Sampler data stays flat on purpose.
fn walk_tree<'a>(
    node: &'a Node<'a>,
    hardware: &mut Vec<String>,
    hardware_id: &mut String,
    group: &mut String,
    out: &mut Vec<SensorItem>,
) {
    if let Some(id) = node.hardware_id {
        hardware.push(node.text.to_string());
        let previous_id = std::mem::replace(hardware_id, id.to_owned());
        let previous_group = std::mem::take(group);
        for child in &node.children {
            walk_tree(child, hardware, hardware_id, group, out);
        }
        hardware.pop();
        *hardware_id = previous_id;
        *group = previous_group;
        return;
    }

    if let Some(sensor_id) = node.sensor_id {
        if let Some((value, unit)) = reading(node) {
            out.push(SensorItem {
                id: format!("lhm:{sensor_id}"),
                text: node.text.to_string(),
                unit,
                value,
                hardware: if hardware.is_empty() {
                    vec!["Other".to_owned()]
                } else {
                    hardware.clone()
                },
                hardware_id: hardware_id.clone(),
                group: if group.is_empty() {
                    "Other".to_owned()
                } else {
                    group.clone()
                },
            });
        }
        return;
    }

    if !node.children.is_empty() {
        let previous = if node.text.is_empty() || node.text.as_ref() == "Sensor" {
            group.clone()
        } else {
            std::mem::replace(group, node.text.to_string())
        };
        for child in &node.children {
            walk_tree(child, hardware, hardware_id, group, out);
        }
        *group = previous;
    }
}

pub fn parse_tree(json: &str) -> Option<Vec<SensorItem>> {
    let root: Node = serde_json::from_str(json).ok()?;
    let mut items = Vec::new();
    let mut hardware = Vec::new();
    let mut hardware_id = String::new();
    let mut group = String::new();
    walk_tree(
        &root,
        &mut hardware,
        &mut hardware_id,
        &mut group,
        &mut items,
    );
    (!items.is_empty()).then_some(items)
}

pub fn fetch_tree() -> Option<Vec<SensorItem>> {
    let mut resp = agent().get(URL).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    parse_tree(&text)
}

fn is_cpu_temp_name(name: &str) -> bool {
    matches!(
        name,
        "CPU Package" | "Core (Tctl/Tdie)" | "Core (Tctl)" | "Core (Tdie)" | "CCDs Max (Tdie)" | "Core Max" | "Core Average"
    )
}

fn is_cpu_power_name(name: &str) -> bool {
    matches!(name, "CPU Package" | "Package")
}

fn apply(sensor_id: &str, name: &str, value: &str, raw: &str, out: &mut LhmSample) {
    let intel_or_amd = sensor_id.starts_with("/intelcpu/") || sensor_id.starts_with("/amdcpu/");
    // LHM 0.9.6 renamed the GPU families to /gpu-nvidia, /gpu-amd, /gpu-intel.
    let gpu = sensor_id.starts_with("/nvidiagpu/")
        || sensor_id.starts_with("/atigpu/")
        || sensor_id.starts_with("/gpu-");

    if intel_or_amd {
        if sensor_id.contains("/temperature/") && is_cpu_temp_name(name) {
            out.cpu_temp = out.cpu_temp.or_else(|| temp(raw));
        }
        if sensor_id.contains("/power/") && is_cpu_power_name(name) {
            out.cpu_power = out.cpu_power.or_else(|| num(value));
        }
    } else if sensor_id.starts_with("/lpc/") {
        if sensor_id.contains("/temperature/") && out.board_temp.is_none() {
            out.board_temp = temp(raw);
        }
        if sensor_id.contains("/fan/") && out.fan_rpm.is_none() {
            out.fan_rpm = num(raw);
        }
    } else if gpu {
        // The path check matters: "GPU Memory" is also a clock and a voltage sensor.
        match name {
            "GPU Hot Spot" if sensor_id.contains("/temperature/") => {
                out.gpu_hotspot = out.gpu_hotspot.or_else(|| temp(raw))
            }
            "GPU Memory Junction" | "GPU Memory" if sensor_id.contains("/temperature/") => {
                out.gpu_mem_temp = out.gpu_mem_temp.or_else(|| temp(raw))
            }
            "D3D Shared Memory Used" => {
                out.gpu_shared_mb = out.gpu_shared_mb.or_else(|| scaled_mb(value))
            }
            _ => {}
        }
        if sensor_id.contains("/fan/") {
            // Prefer the GPU fan over the board fan that was seen first.
            if let Some(rpm) = num(raw) {
                out.fan_rpm = Some(rpm);
            }
        }
    }
}

/// Temperatures: `RawValue` is always Celsius even when LHM is set to Fahrenheit.
/// If a value still carries °F, convert it rather than mislabel it as °C.
fn temp(text: &str) -> Option<f64> {
    let value = num(text)?;
    if text.contains("°F") || text.to_ascii_uppercase().contains(" F ") {
        Some((value - 32.0) * 5.0 / 9.0)
    } else {
        Some(value)
    }
}

/// LHM scales large values to GB, so `1,2 GB` must not be read as 1.2 MB.
fn scaled_mb(text: &str) -> Option<f64> {
    let value = num(text)?;
    if text.to_ascii_uppercase().contains("GB") {
        Some(value * 1024.0)
    } else {
        Some(value)
    }
}

/// LHM formats numbers with the current locale (`"35,0 °C"`) and units in the same
/// string. Take the leading number and normalise the decimal separator.
fn num(text: &str) -> Option<f64> {
    let cleaned = text.trim().replace(',', ".");
    let start = cleaned
        .char_indices()
        .find(|(_, c)| c.is_ascii_digit() || *c == '-' || *c == '.')
        .map(|(i, _)| i)?;
    let tail = &cleaned[start..];
    let end = tail
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == 'e' || *c == 'E'))
        .map(|(i, _)| i)
        .unwrap_or(tail.len());
    tail[..end].parse::<f64>().ok()
}

/// Value and unit from one sensor node; the flat `walk` and `walk_tree` share it.
fn reading(node: &Node<'_>) -> Option<(f64, String)> {
    let raw = if node.raw.is_empty() {
        node.value.as_ref()
    } else {
        node.raw.as_ref()
    };
    let value = num(raw)?;
    let unit = node
        .value
        .trim()
        .rsplit_once(' ')
        .map(|(_, unit)| unit.trim().to_owned())
        .unwrap_or_default();
    Some((value, unit))
}

/// Reads and parses repeatedly while reusing one string buffer and one map, so a
/// poll allocates almost nothing after the first one.
pub struct Reader {
    text: String,
    data: LhmData,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            data: LhmData::default(),
        }
    }

    pub fn read(&mut self) -> Option<&LhmData> {
        use std::io::Read;

        let mut resp = agent().get(URL).call().ok()?;
        self.text.clear();
        resp.body_mut()
            .as_reader()
            .read_to_string(&mut self.text)
            .ok()?;

        self.data.named = LhmSample::default();
        self.data.all.clear();
        let root: Node = serde_json::from_str(&self.text).ok()?;
        let mut sensors = 0usize;
        walk(&root, &mut self.data, &mut sensors);
        (sensors > 0).then_some(&self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_and_board_sensors_by_id() {
        let json = r#"{"Children":[
          {"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
            {"Text":"Temperatures","Children":[
              {"Text":"CPU Package","SensorId":"/intelcpu/0/temperature/4","Value":"35,0 °C"}]},
            {"Text":"Powers","Children":[
              {"Text":"CPU Package","SensorId":"/intelcpu/0/power/0","Value":"63,3 W"}]}]},
          {"Text":"Board","HardwareId":"/lpc/nct6791d/0","Children":[
            {"Text":"Temperatures","Children":[
              {"Text":"Temperature #1","SensorId":"/lpc/nct6791d/0/temperature/0","Value":"42,5 °C"}]}]}]}"#;
        let s = parse(json).unwrap();
        assert_eq!(s.cpu_temp, Some(35.0));
        assert_eq!(s.cpu_power, Some(63.3));
        assert_eq!(s.board_temp, Some(42.5));
    }

    #[test]
    fn parses_gpu_extras_and_shared_memory() {
        let json = r#"{"Children":[{"Text":"GPU","HardwareId":"/nvidiagpu/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"GPU Hot Spot","SensorId":"/nvidiagpu/0/temperature/2","Value":"72,3 °C"},
            {"Text":"GPU Memory Junction","SensorId":"/nvidiagpu/0/temperature/3","Value":"66,0 °C"}]},
          {"Text":"Data","Children":[
            {"Text":"D3D Shared Memory Used","SensorId":"/nvidiagpu/0/data/7","Value":"221,9 MB"}]},
          {"Text":"Fans","Children":[
            {"Text":"GPU Fan 1","SensorId":"/nvidiagpu/0/fan/0","Value":"1243 RPM"}]}]}]}"#;
        let s = parse(json).unwrap();
        assert_eq!(s.gpu_hotspot, Some(72.3));
        assert_eq!(s.gpu_mem_temp, Some(66.0));
        assert_eq!(s.gpu_shared_mb, Some(221.9));
        assert_eq!(s.fan_rpm, Some(1243.0));
    }

    #[test]
    fn parses_escaped_degree_values() {
        let json = r#"{"Children":[{"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"CPU Package","SensorId":"/intelcpu/0/temperature/0","Value":"39,0 \u00B0C","RawValue":"39,0 \u00B0C"}
          ]}]}]}"#;
        let s = parse(json).unwrap();
        assert_eq!(s.cpu_temp, Some(39.0));
    }

    #[test]
    fn amd_ids_are_matched_too() {
        let json = r#"{"Children":[{"Text":"CPU","HardwareId":"/amdcpu/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"Core (Tctl/Tdie)","SensorId":"/amdcpu/0/temperature/2","Value":"61.0 °C"}]},
          {"Text":"Powers","Children":[
            {"Text":"Package","SensorId":"/amdcpu/0/power/0","Value":"88.5 W"}]}]}]}"#;
        let s = parse(json).unwrap();
        assert_eq!(s.cpu_temp, Some(61.0));
        assert_eq!(s.cpu_power, Some(88.5));
    }

    #[test]
    fn gb_scaled_shared_memory_is_converted_to_mb() {
        let json = r#"{"Children":[{"Text":"G","HardwareId":"/nvidiagpu/0","Children":[
          {"Text":"Data","Children":[
            {"Text":"D3D Shared Memory Used","SensorId":"/nvidiagpu/0/data/7","Value":"1,2 GB"}]}]}]}"#;
        let s = parse(json).unwrap();
        assert!((s.gpu_shared_mb.unwrap() - 1228.8).abs() < 0.01);
    }

    #[test]
    fn missing_sensors_are_none_not_errors() {
        let s = parse(
            r#"{"Children":[{"Text":"X","HardwareId":"/lpc/acme/0","Children":[
              {"Text":"Voltages","Children":[
                {"Text":"Vcore","SensorId":"/lpc/acme/0/voltage/0","Value":"1,0 V"}]}]}]}"#,
        )
        .unwrap();
        assert_eq!(s.cpu_temp, None);
        assert_eq!(s.board_temp, None);
    }

    #[test]
    #[ignore = "binds port 8085"]
    fn fetch_reads_a_loopback_server() {
        use std::io::{Read, Write};

        let body = r#"{"Children":[{"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"CPU Package","SensorId":"/intelcpu/0/temperature/4","Value":"35,0 °C"}]}]}]}"#;

        let listener = std::net::TcpListener::bind("127.0.0.1:8085").expect("port 8085 busy");
        let body_owned = body.to_owned();
        let handle = std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_owned.len(),
                    body_owned
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });

        let data = fetch_data().expect("did not read the loopback server");
        let _ = handle.join();
        assert_eq!(data.named.cpu_temp, Some(35.0));
        assert!(data.all.contains_key("lhm:/intelcpu/0/temperature/4"));
    }

    #[test]
    fn exposes_every_numeric_sensor_with_its_unit() {
        let json = r#"{"Children":[{"Text":"G","HardwareId":"/nvidiagpu/0","Children":[
          {"Text":"Clocks","Children":[
            {"Text":"GPU Core","SensorId":"/nvidiagpu/0/clock/0","Value":"225,0 MHz"}]},
          {"Text":"Temperatures","Children":[
            {"Text":"GPU Core","SensorId":"/nvidiagpu/0/temperature/0","Value":"52,0 °C"}]}]}]}"#;
        let all = parse_data(json).unwrap().all;
        let clk = all.get("lhm:/nvidiagpu/0/clock/0").unwrap();
        assert_eq!(clk.value, 225.0);
        assert_eq!(clk.unit, "MHz");
        assert_eq!(all.get("lhm:/nvidiagpu/0/temperature/0").unwrap().unit, "°C");
    }

    #[test]
    fn memory_clock_is_not_read_as_memory_temperature() {
        // LHM emits Voltage/Clock before Temperature for the same "GPU Memory" text.
        let json = r#"{"Children":[{"Text":"G","HardwareId":"/nvidiagpu/0","Children":[
          {"Text":"Voltages","Children":[
            {"Text":"GPU Memory","SensorId":"/nvidiagpu/0/voltage/1","Value":"1,35 V"}]},
          {"Text":"Clocks","Children":[
            {"Text":"GPU Memory","SensorId":"/nvidiagpu/0/clock/1","Value":"7001 MHz"}]},
          {"Text":"Temperatures","Children":[
            {"Text":"GPU Memory Junction","SensorId":"/nvidiagpu/0/temperature/3","Value":"66,0 °C"}]}]}]}"#;
        assert_eq!(parse(json).unwrap().gpu_mem_temp, Some(66.0));
    }

    #[test]
    fn fahrenheit_is_converted_to_celsius() {
        let json = r#"{"Children":[{"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"CPU Package","SensorId":"/intelcpu/0/temperature/4","Value":"140,0 °F","RawValue":"60,0 °C"}]}]}]}"#;
        assert_eq!(parse(json).unwrap().cpu_temp, Some(60.0));
    }

    #[test]
    fn gpu_nvidia_prefix_is_matched() {
        // LHM 0.9.6 uses /gpu-nvidia/0 instead of /nvidiagpu/0.
        let json = r#"{"Children":[{"Text":"G","HardwareId":"/gpu-nvidia/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"GPU Hot Spot","SensorId":"/gpu-nvidia/0/temperature/2","Value":"69,7 °C"},
            {"Text":"GPU Memory Junction","SensorId":"/gpu-nvidia/0/temperature/3","Value":"62,0 °C"}]},
          {"Text":"Fans","Children":[
            {"Text":"GPU Fan 1","SensorId":"/gpu-nvidia/0/fan/0","Value":"1243 RPM"}]}]}]}"#;
        let s = parse(json).unwrap();
        assert_eq!(s.gpu_hotspot, Some(69.7));
        assert_eq!(s.gpu_mem_temp, Some(62.0));
        assert_eq!(s.fan_rpm, Some(1243.0));
    }

    #[test]
    fn unrelated_json_is_not_connected() {
        assert!(parse(r#"{"foo":"bar"}"#).is_none());
        assert!(parse("{}").is_none());
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse("not json").is_none());
    }

    #[test]
    fn na_values_do_not_become_zero() {
        let json = r#"{"Children":[{"Text":"G","HardwareId":"/nvidiagpu/0","Children":[
          {"Text":"GPU Hot Spot","SensorId":"/nvidiagpu/0/temperature/2","Value":"[N/A]"}]}]}"#;
        assert_eq!(parse(json).unwrap().gpu_hotspot, None);
    }

    #[test]
    fn tree_keeps_hardware_group_and_units() {
        let json = r#"{"Children":[
          {"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
            {"Text":"Temperatures","Children":[
              {"Text":"CPU Package","SensorId":"/intelcpu/0/temperature/4","Value":"35,0 °C"}]}]},
          {"Text":"GPU","HardwareId":"/gpu-nvidia/0","Children":[
            {"Text":"Temperatures","Children":[
              {"Text":"GPU Hot Spot","SensorId":"/gpu-nvidia/0/temperature/2","Value":"72,3 °C"}]}]}]}"#;

        let items = parse_tree(json).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "lhm:/intelcpu/0/temperature/4");
        assert_eq!(items[0].text, "CPU Package");
        assert_eq!(items[0].unit, "°C");
        assert_eq!(items[0].value, 35.0);
        assert_eq!(items[0].hardware, vec!["CPU"]);
        assert_eq!(items[0].hardware_id, "/intelcpu/0");
        assert_eq!(items[0].group, "Temperatures");
        assert_eq!(items[1].hardware, vec!["GPU"]);
    }

    #[test]
    fn tree_keeps_nested_board_path() {
        let json = r#"{"Children":[
          {"Text":"Board","HardwareId":"/motherboard","Children":[
            {"Text":"Nuvoton NCT6791D","HardwareId":"/lpc/nct6791d/0","Children":[
              {"Text":"Temperatures","Children":[
                {"Text":"Temperature #1","SensorId":"/lpc/nct6791d/0/temperature/0","Value":"42,5 °C"}]}]}]}]}"#;

        let items = parse_tree(json).unwrap();
        assert_eq!(items[0].hardware, vec!["Board", "Nuvoton NCT6791D"]);
        assert_eq!(items[0].hardware_id, "/lpc/nct6791d/0");
        assert_eq!(items[0].group, "Temperatures");
    }

    #[test]
    fn tree_skips_non_numeric_sensors() {
        let json = r#"{"Children":[
          {"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
            {"Text":"Temperatures","Children":[
              {"Text":"N/A","SensorId":"/intelcpu/0/temperature/9","Value":"N/A"}]}]}]}"#;

        assert!(parse_tree(json).is_none());
    }
}
