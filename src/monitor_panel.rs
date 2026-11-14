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

fn numeric(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Num(n)) => Some(*n),
        Some(Value::UsedTotal(used, _)) => Some(*used),
        _ => None,
    }
}

fn draw_field(ui: &mut egui::Ui, cfg: &Config, def: &FieldDef, sample: &Sample) {
    let value = sample.fields.get(def.id);
    let shown = format_value(def, value, sample);
    let text = if cfg.monitor.show_labels && !def.label.is_empty() {
        format!("{} {shown}", def.label)
    } else {
        shown
    };

    let color = if def.id == "model" || def.id == "alias" {
        cfg.theme.accent
    } else {
        numeric(value)
            .map(|n| color_for(cfg.thresholds.get(def.id), n, &cfg.theme))
            .unwrap_or(cfg.theme.label)
    };

    let style = cfg
        .monitor
        .field_style
        .get(def.id)
        .copied()
        .unwrap_or(Style::Text);

    match style {
        Style::Text => {
            ui.label(
                egui::RichText::new(text)
                    .size(cfg.monitor.font_size)
                    .color(color),
            );
        }
        // Bars and graphs land in the next task; text keeps the panel correct meanwhile.
        Style::Bar | Style::Graph | Style::GraphText => {
            ui.label(
                egui::RichText::new(text)
                    .size(cfg.monitor.font_size)
                    .color(color),
            );
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
                ui.set_width((cfg.monitor.width - 2.0 * pad).max(40.0));
            }
            ui.spacing_mut().item_spacing.y = 3.0;
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
        assert_eq!(format_value(field_def("temp").unwrap(), None, &s), "--");
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
}
