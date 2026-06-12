use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use plotters::prelude::*;

/// Один ряд данных на графике.
pub struct Series {
    pub label: String,
    pub color: RGBColor,
    pub points: Vec<(NaiveDate, u64)>,
}

pub const COLOR_REGULAR: RGBColor = RGBColor(0, 90, 220);
pub const COLOR_REGULAR_OLD: RGBColor = RGBColor(130, 175, 255);
pub const COLOR_HOT: RGBColor = RGBColor(220, 30, 30);
pub const COLOR_HOT_OLD: RGBColor = RGBColor(255, 160, 90);

/// Регистрирует вшитый в бинарь шрифт (бэкенд ab_glyph не видит системные шрифты).
fn ensure_fonts() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let font: &'static [u8] = include_bytes!("../assets/DejaVuSans.ttf");
        for style in [
            plotters::style::FontStyle::Normal,
            plotters::style::FontStyle::Bold,
            plotters::style::FontStyle::Italic,
        ] {
            if plotters::style::register_font("sans-serif", style, font).is_err() {
                eprintln!("не удалось зарегистрировать шрифт графиков");
                return;
            }
        }
    });
}

/// Рисует несколько ценовых рядов с легендой и возвращает PNG.
/// Пустые ряды пропускаются; деления по оси X — на каждый день.
pub fn render_chart(title: &str, series: &[Series]) -> Result<Vec<u8>> {
    ensure_fonts();
    let series: Vec<&Series> = series.iter().filter(|s| !s.points.is_empty()).collect();
    anyhow::ensure!(!series.is_empty(), "нет точек для графика");

    let all: Vec<(NaiveDate, u64)> = series.iter().flat_map(|s| s.points.iter().copied()).collect();
    let min_date = all.iter().map(|p| p.0).min().unwrap();
    let max_date = all.iter().map(|p| p.0).max().unwrap();
    let min_price = all.iter().map(|p| p.1).min().unwrap();
    let max_price = all.iter().map(|p| p.1).max().unwrap();
    let pad = ((max_price - min_price) / 10).max(2000);

    let path = std::env::temp_dir().join(format!(
        "tv_chart_{}_{}.png",
        std::process::id(),
        all.len()
    ));
    {
        let root = BitMapBackend::new(&path, (1100, 560)).into_drawing_area();
        root.fill(&WHITE).map_err(|e| anyhow!("{e}"))?;

        let days = (max_date - min_date).num_days().max(1) as usize + 1;
        let mut chart = ChartBuilder::on(&root)
            .margin(24)
            .caption(title, ("sans-serif", 24))
            .x_label_area_size(58)
            .y_label_area_size(80)
            .build_cartesian_2d(min_date..max_date, min_price.saturating_sub(pad)..max_price + pad)
            .map_err(|e| anyhow!("{e}"))?;

        chart
            .configure_mesh()
            .x_labels(days.min(46))
            .y_labels(8)
            .x_label_formatter(&|d| d.format("%d.%m").to_string())
            .y_label_formatter(&|p| format!("{} тыс.", p / 1000))
            .label_style(("sans-serif", 14))
            .draw()
            .map_err(|e| anyhow!("{e}"))?;

        for s in &series {
            let color = s.color;
            chart
                .draw_series(LineSeries::new(s.points.iter().copied(), &color))
                .map_err(|e| anyhow!("{e}"))?
                .label(&s.label)
                .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color));
            chart
                .draw_series(s.points.iter().map(|&(d, p)| Circle::new((d, p), 3, color.filled())))
                .map_err(|e| anyhow!("{e}"))?;
        }

        chart
            .configure_series_labels()
            .position(SeriesLabelPosition::UpperRight)
            .background_style(WHITE.mix(0.85))
            .border_style(BLACK.mix(0.4))
            .label_font(("sans-serif", 16))
            .draw()
            .map_err(|e| anyhow!("{e}"))?;

        root.present().map_err(|e| anyhow!("{e}"))?;
    }

    let bytes = std::fs::read(&path).context("не прочитал PNG графика")?;
    let _ = std::fs::remove_file(&path);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, d).unwrap()
    }

    #[test]
    fn renders_two_series() {
        let series = [
            Series {
                label: "Обычные туры".into(),
                color: COLOR_REGULAR,
                points: (1..=28).map(|d| (date(d), 140_000 + (d as u64 * 7 % 9) * 9_000)).collect(),
            },
            Series {
                label: "Горящие туры".into(),
                color: COLOR_HOT,
                points: [3u32, 6, 9, 14, 20].iter().map(|&d| (date(d), 95_000 + d as u64 * 2_000)).collect(),
            },
        ];
        let png = render_chart("Цены по дням: обычные vs горящие", &series).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::write("/tmp/test_chart.png", &png).unwrap();
    }

    #[test]
    fn renders_retro_four_series() {
        let series = [
            Series {
                label: "Обычные: было".into(),
                color: COLOR_REGULAR_OLD,
                points: (1..=20).map(|d| (date(d), 175_000 + (d as u64 % 5) * 6_000)).collect(),
            },
            Series {
                label: "Обычные: сейчас".into(),
                color: COLOR_REGULAR,
                points: (1..=20).map(|d| (date(d), 160_000 + (d as u64 % 5) * 6_000)).collect(),
            },
            Series {
                label: "Горящие: было".into(),
                color: COLOR_HOT_OLD,
                points: [4u32, 9, 15].iter().map(|&d| (date(d), 150_000 + d as u64 * 1_000)).collect(),
            },
            Series {
                label: "Горящие: сейчас".into(),
                color: COLOR_HOT,
                points: [4u32, 9, 15].iter().map(|&d| (date(d), 120_000 + d as u64 * 1_000)).collect(),
            },
        ];
        let png = render_chart("Ретроспектива цен", &series).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::write("/tmp/test_retro.png", &png).unwrap();
    }
}
