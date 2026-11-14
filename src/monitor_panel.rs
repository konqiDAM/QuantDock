//! Renders the overlay panel from a config and a sample. Shared by the overlay and
//! the settings preview so both always agree.

use crate::monitor_config::{
    field_def, Config, Direction, FieldDef, FieldKind, Row, Style, Threshold, Theme,
};
use crate::monitor_sample::{Sample, Value};
use eframe::egui;

pub fn visible_fields(row: &Row) -> Vec<&'static FieldDef> {
    row.fields.iter().filter_map(|id| field_def(id)).collect()
}

pub fn format_value(def: &FieldDef, value: Option<&Value>, _sample: &Sample) -> String {
    let Some(value) = value else {
        return "--".to_owned();
    };
    match (def.kind, value) {
        (FieldKind::Text, Value::Text(t)) => t.clone(),
        (FieldKind::Text, Value::Num(n)) => format!("{n:.0}"),
        (FieldKind::Percent, Value::Num(n)) => format!("{n:.0}%"),
        (FieldKind::Gb, Value::UsedTotal(used, total)) => {
            format!("{:.1}/{:.1} GB", *used / 1024.0, *total / 1024.0)
        }
        (FieldKind::Gb, Value::Num(n)) => format!("{:.0} GB", *n / 1024.0),
        (FieldKind::Megabytes, Value::Num(n)) => format!("{n:.0} MB"),
        (FieldKind::Rpm, Value::Num(n)) => format!("{n:.0} RPM"),
        (FieldKind::Celsius, Value::Num(n)) => format!("{n:.0} °C"),
        (FieldKind::Watts, Value::Num(n)) => format!("{n:.0} W"),
        (FieldKind::Mhz, Value::Num(n)) => format!("{n:.0} MHz"),
        (FieldKind::Count, Value::Num(n)) => format!("{n:.0}"),
        (FieldKind::Number, Value::Num(n)) if *n < 100.0 => format!("{n:.0}"),
        (FieldKind::Number, Value::Num(n)) => format!("{n:.1}"),
        _ => "--".to_owned(),
    }
}

pub fn color_for(threshold: Option<&Threshold>, value: f64, theme: &Theme) -> egui::Color32 {
    let Some(t) = threshold else {
        return theme.text;
    };
    match t.direction {
        Direction::Above => {
            if t.crit.is_some_and(|c| value >= c) {
                theme.crit
            } else if t.warn.is_some_and(|w| value >= w) {
                theme.warn
            } else {
                theme.text
            }
        }
        Direction::Below => {
            if t.crit.is_some_and(|c| value <= c) {
                theme.crit
            } else if t.warn.is_some_and(|w| value <= w) {
                theme.warn
            } else {
                theme.text
            }
        }
    }
}

/// The filled part of a bar. Clamped so it can never leave `content`, which is why
/// bars keep their right margin at 0 % and at 100 %.
pub fn bar_rect(content: egui::Rect, frac: f64) -> egui::Rect {
    let f = frac.clamp(0.0, 1.0) as f32;
    egui::Rect::from_min_size(
        content.min,
        egui::vec2(content.width() * f, content.height()),
    )
}

/// Points for a sparkline: oldest at the left edge, newest at the right, value 0 at
/// the bottom and `max` at the top. Clamped, so out-of-range samples stay inside.
/// With no history it draws nothing; the caller falls back to text.
pub fn spark_points(history: &[f64], max: f64, rect: egui::Rect) -> Vec<egui::Pos2> {
    if history.is_empty() {
        return Vec::new();
    }
    let max = if max.is_finite() && max > 0.0 { max } else { 1.0 };
    let y_for = |v: f64| {
        let f = (v / max).clamp(0.0, 1.0) as f32;
        rect.bottom() - f * rect.height()
    };
    if history.len() == 1 {
        let y = y_for(history[0]);
        return vec![
            egui::pos2(rect.left(), y),
            egui::pos2(rect.right(), y),
        ];
    }
    let step = rect.width() / (history.len() - 1) as f32;
    history
        .iter()
        .enumerate()
        .map(|(i, v)| egui::pos2(rect.left() + step * i as f32, y_for(*v)))
        .collect()
}

/// Numeric value in the units thresholds are written in: GB for memory, otherwise
/// the raw sample value.
fn threshold_value(def: &FieldDef, value: Option<&Value>) -> Option<f64> {
    match (def.kind, value?) {
        (FieldKind::Gb, Value::UsedTotal(used, _)) => Some(*used / 1024.0),
        (FieldKind::Gb, Value::Num(mb)) => Some(*mb / 1024.0),
        (_, Value::Num(n)) => Some(*n),
        _ => None,
    }
}

/// Filled fraction for bar styles. Only fields with a natural 0..1 scale qualify:
/// percentages and used/total pairs. Everything else falls back to text.
fn bar_fraction(def: &FieldDef, value: Option<&Value>) -> Option<f64> {
    match (def.kind, value?) {
        (FieldKind::Percent, Value::Num(n)) => Some(n / 100.0),
        (FieldKind::Gb, Value::UsedTotal(used, total)) if *total > 0.0 => Some(used / total),
        _ => None,
    }
}

fn draw_field(ui: &mut egui::Ui, cfg: &Config, def: &FieldDef, sample: &Sample) {
    let value = sample.fields.get(def.id);
    let shown = format_value(def, value, sample);
    let size = cfg.monitor.font_size;

    let value_color = if def.id == "model" || def.id == "alias" {
        cfg.theme.accent
    } else {
        threshold_value(def, value)
            .map(|n| color_for(cfg.thresholds.get(def.id), n, &cfg.theme))
            .unwrap_or(cfg.theme.text)
    };

    // Only the value carries the threshold colour; the label stays neutral.
    if cfg.monitor.show_labels && !def.label.is_empty() {
        ui.label(
            egui::RichText::new(def.label)
                .size(size)
                .color(cfg.theme.label),
        );
    }

    let style = cfg
        .monitor
        .field_style
        .get(def.id)
        .copied()
        .unwrap_or(Style::Text);
    // Text-only fields (model, pstate, ...) can never be a bar or a graph.
    let style = if matches!(def.kind, FieldKind::Text) {
        Style::Text
    } else {
        style
    };

    let width_cap = ui.available_width().max(size * 4.0);
    let height = size * 1.3;

    match style {
        Style::Bar if bar_fraction(def, value).is_some() => {
            let frac = bar_fraction(def, value).unwrap_or(0.0);
            let width = (size * 7.0).min(width_cap);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 4.0, egui::Color32::from_white_alpha(18));
            let fill = bar_rect(rect.shrink(1.0), frac);
            let tint =
                egui::Color32::from_rgba_unmultiplied(value_color.r(), value_color.g(), value_color.b(), 150);
            ui.painter().rect_filled(fill, 3.0, tint);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &shown,
                egui::FontId::proportional(size),
                cfg.theme.text,
            );
        }
        Style::Graph | Style::GraphText => {
            let history = sample
                .history
                .get(def.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let max = match (def.kind, value) {
                (FieldKind::Percent, _) => 100.0,
                (FieldKind::Gb, Some(Value::UsedTotal(_, total))) => (*total / 1024.0).max(1.0),
                _ => history.iter().cloned().fold(1.0f64, f64::max),
            };
            let graph_w = (if style == Style::Graph {
                size * 9.0
            } else {
                size * 7.0
            })
            .min(width_cap);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(graph_w, height), egui::Sense::hover());
            let pts = spark_points(history, max, rect.shrink(1.0));
            if pts.len() >= 2 {
                ui.painter()
                    .add(egui::Shape::line(pts, egui::Stroke::new(1.0, value_color)));
            }
            if style == Style::GraphText {
                ui.label(egui::RichText::new(shown).size(size).color(value_color));
            }
        }
        // Text, plus bar/graph styles on fields with no natural scale.
        _ => {
            ui.label(egui::RichText::new(shown).size(size).color(value_color));
        }
    }
}

/// Draws the panel and returns the size the window should be.
pub fn render(ui: &mut egui::Ui, cfg: &Config, sample: &Sample) -> egui::Vec2 {
    let full = ui.max_rect();
    let [r, g, b, _] = cfg.theme.background.to_array();
    ui.painter().rect_filled(
        full,
        cfg.monitor.corner_radius,
        egui::Color32::from_rgba_unmultiplied(r, g, b, cfg.monitor.background_alpha),
    );

    let pad = 12.0;
    let frame = egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(pad as i8, 8))
        .show(ui, |ui| {
            if cfg.monitor.width > 0.0 {
                ui.set_width((cfg.monitor.width - 2.0 * pad).max(20.0));
            }
            ui.spacing_mut().item_spacing.y = 3.0;
            if !cfg.monitor.title.is_empty() {
                ui.label(
                    egui::RichText::new(&cfg.monitor.title)
                        .size(cfg.monitor.font_size)
                        .color(cfg.theme.accent),
                );
            }
            for row in &cfg.monitor.rows {
                let fields = visible_fields(row);
                if fields.is_empty() {
                    ui.add_space(6.0);
                    continue;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for def in fields {
                        draw_field(ui, cfg, def, sample);
                    }
                });
            }
        });

    frame.response.rect.size()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor_config::{field_def, Direction, Row, Theme, Threshold};
    use crate::monitor_sample::{Sample, Value};

    #[test]
    fn formats_percent_and_bytes() {
        let mut s = Sample::default();
        s.fields.insert("gpu", Value::Num(15.0));
        assert_eq!(
            format_value(field_def("gpu").unwrap(), s.fields.get("gpu"), &s),
            "15%"
        );

        s.fields.insert("vram", Value::UsedTotal(3.2 * 1024.0, 24.0 * 1024.0));
        assert_eq!(
            format_value(field_def("vram").unwrap(), s.fields.get("vram"), &s),
            "3.2/24.0 GB"
        );
    }

    #[test]
    fn missing_value_renders_dashes() {
        let s = Sample::default();
        assert_eq!(format_value(field_def("gpu_temp").unwrap(), None, &s), "--");
    }

    #[test]
    fn thresholds_pick_colours() {
        let theme = Theme::default();
        let t = Threshold {
            warn: Some(70.0),
            crit: Some(82.0),
            direction: Direction::Above,
        };
        assert_eq!(color_for(Some(&t), 52.0, &theme), theme.text);
        assert_eq!(color_for(Some(&t), 70.0, &theme), theme.warn);
        assert_eq!(color_for(Some(&t), 82.0, &theme), theme.crit);
        assert_eq!(color_for(None, 999.0, &theme), theme.text);
    }

    #[test]
    fn below_thresholds_invert() {
        let theme = Theme::default();
        let t = Threshold {
            warn: Some(100.0),
            crit: Some(40.0),
            direction: Direction::Below,
        };
        assert_eq!(color_for(Some(&t), 400.0, &theme), theme.text);
        assert_eq!(color_for(Some(&t), 100.0, &theme), theme.warn);
        assert_eq!(color_for(Some(&t), 40.0, &theme), theme.crit);
    }

    #[test]
    fn bar_never_exceeds_its_content_rect() {
        let content = egui::Rect::from_min_size(egui::pos2(10.0, 5.0), egui::vec2(200.0, 18.0));
        assert_eq!(bar_rect(content, 0.0).width(), 0.0);
        assert_eq!(bar_rect(content, 1.0).right(), content.right());
        assert!(
            bar_rect(content, 2.0).right() <= content.right(),
            "clamped above 1.0"
        );
        assert!(bar_rect(content, -1.0).width() >= 0.0, "clamped below 0.0");
    }

    #[test]
    fn unknown_fields_are_dropped_not_rendered() {
        let row = Row {
            fields: vec!["gpu".into(), "not_a_field".into(), "cpu".into()],
        };
        let visible: Vec<&str> = visible_fields(&row).iter().map(|d| d.id).collect();
        assert_eq!(visible, vec!["gpu", "cpu"]);
    }

    #[test]
    fn disk_free_threshold_is_read_in_gb() {
        let def = field_def("disk_free").unwrap();
        // 50 GB free, expressed in the MB the sampler emits.
        let fifty = Some(Value::Num(50.0 * 1024.0));
        assert_eq!(threshold_value(def, fifty.as_ref()), Some(50.0));

        let t = Threshold {
            warn: Some(100.0),
            crit: Some(40.0),
            direction: Direction::Below,
        };
        let theme = Theme::default();
        assert_eq!(
            color_for(Some(&t), threshold_value(def, fifty.as_ref()).unwrap(), &theme),
            theme.warn,
            "50 GB free must warn against the default 100 GB threshold"
        );
        let thirty = Some(Value::Num(30.0 * 1024.0));
        assert_eq!(
            color_for(Some(&t), threshold_value(def, thirty.as_ref()).unwrap(), &theme),
            theme.crit
        );
    }

    #[test]
    fn vram_bar_and_threshold_use_used_gb() {
        let def = field_def("vram").unwrap();
        let v = Some(Value::UsedTotal(20.0 * 1024.0, 24.0 * 1024.0));
        let frac = bar_fraction(def, v.as_ref()).unwrap();
        assert!((frac - 20.0 / 24.0).abs() < 1e-6, "bar must be used/total");
        assert_eq!(threshold_value(def, v.as_ref()), Some(20.0));
    }

    #[test]
    fn unscaled_fields_have_no_bar() {
        assert_eq!(
            bar_fraction(field_def("gpu_temp").unwrap(), Some(&Value::Num(52.0))),
            None
        );
        assert_eq!(
            bar_fraction(field_def("disk_free").unwrap(), Some(&Value::Num(50.0 * 1024.0))),
            None
        );
    }

    #[test]
    fn spark_starts_at_left_and_ends_at_right() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
        let pts = spark_points(&[0.0, 50.0, 100.0], 100.0, rect);
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0].x, rect.left());
        assert_eq!(pts[2].x, rect.right());
        assert_eq!(pts[0].y, rect.bottom());
        assert_eq!(pts[2].y, rect.top());
    }

    #[test]
    fn spark_with_one_point_is_flat_not_empty() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
        let pts = spark_points(&[42.0], 100.0, rect);
        assert_eq!(pts.len(), 2, "one sample is drawn as a flat two-point line");
        assert_eq!(pts[0].y, pts[1].y);
    }

    #[test]
    fn spark_with_no_points_is_empty() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
        assert!(spark_points(&[], 100.0, rect).is_empty());
    }

    #[test]
    fn spark_clamps_out_of_range_values() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
        let pts = spark_points(&[-50.0, 500.0], 100.0, rect);
        assert!(pts.iter().all(|p| rect.contains(*p)));
    }
}
