//! LibreHardwareMonitor integration: read its web server JSON.
//!
//! Sensors are matched by `SensorId` / `HardwareId` prefixes
//! (`/intelcpu`, `/amdcpu`, `/nvidiagpu`, `/atigpu`, `/lpc`), never by hardware
//! display names, so the mapping works on any machine.

use serde_json::Value;

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

/// True when a sensor exists only behind LibreHardwareMonitor.
pub fn field_needs_lhm(id: &str) -> bool {
    matches!(
        id,
        "cpu_temp" | "cpu_power" | "board_temp" | "gpu_hotspot" | "gpu_mem_temp" | "gpu_shared"
            | "fan_rpm"
    )
}

pub fn fetch() -> Option<LhmSample> {
    let agent = ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_millis(900)))
        .timeout_connect(Some(std::time::Duration::from_millis(250)))
        .build()
        .new_agent();
    let mut resp = agent.get(URL).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    parse(&text)
}

pub fn parse(json: &str) -> Option<LhmSample> {
    let root: Value = serde_json::from_str(json).ok()?;
    let mut out = LhmSample::default();
    let mut sensors = 0usize;
    walk(&root, None, &mut out, &mut sensors);
    // Unrelated JSON on that port must not look like a working sensor service.
    (sensors > 0).then_some(out)
}

fn walk(node: &Value, hardware: Option<&str>, out: &mut LhmSample, sensors: &mut usize) {
    let hw = node
        .get("HardwareId")
        .and_then(Value::as_str)
        .or(hardware)
        .map(str::to_owned);
    if let Some(id) = node.get("SensorId").and_then(Value::as_str) {
        *sensors += 1;
        let name = node.get("Text").and_then(Value::as_str).unwrap_or("");
        let value = node.get("Value").and_then(Value::as_str).unwrap_or("");
        let raw = node.get("RawValue").and_then(Value::as_str).unwrap_or(value);
        apply(id, name, value, raw, out);
    }
    if let Some(children) = node.get("Children").and_then(Value::as_array) {
        for child in children {
            walk(child, hw.as_deref(), out, sensors);
        }
    }
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
    let gpu = sensor_id.starts_with("/nvidiagpu/") || sensor_id.starts_with("/atigpu/");

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

        let sample = fetch().expect("fetch did not read the loopback server");
        let _ = handle.join();
        assert_eq!(sample.cpu_temp, Some(35.0));
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
}
