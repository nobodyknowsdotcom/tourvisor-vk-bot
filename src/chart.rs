use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use plotters::prelude::*;

/// Рисует наложение двух ценовых рядов (обычные и горящие туры) и возвращает PNG.
pub fn render_overlay_chart(
    regular: &[(NaiveDate, u64)],
    hot: &[(NaiveDate, u64)],
    title: &str,
) -> Result<Vec<u8>> {
    anyhow::ensure!(!regular.is_empty() || !hot.is_empty(), "нет точек для графика");

    let all: Vec<(NaiveDate, u64)> = regular.iter().chain(hot.iter()).copied().collect();
    let min_date = all.iter().map(|p| p.0).min().unwrap();
    let max_date = all.iter().map(|p| p.0).max().unwrap();
    let min_price = all.iter().map(|p| p.1).min().unwrap();
    let max_price = all.iter().map(|p| p.1).max().unwrap();
    let pad = ((max_price - min_price) / 10).max(2000);

    let path = std::env::temp_dir().join(format!("tv_overlay_{}.png", std::process::id()));
    {
        let root = BitMapBackend::new(&path, (1000, 560)).into_drawing_area();
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
            .x_labels(days)
            .y_labels(8)
            .x_label_formatter(&|d| d.format("%d.%m").to_string())
            .y_label_formatter(&|p| format!("{} тыс.", p / 1000))
            .label_style(("sans-serif", 15))
            .draw()
            .map_err(|e| anyhow!("{e}"))?;

        chart
            .draw_series(LineSeries::new(regular.iter().copied(), &BLUE))
            .map_err(|e| anyhow!("{e}"))?
            .label("Обычные туры")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], BLUE));
        chart
            .draw_series(regular.iter().map(|&(d, p)| Circle::new((d, p), 4, BLUE.filled())))
            .map_err(|e| anyhow!("{e}"))?;

        chart
            .draw_series(LineSeries::new(hot.iter().copied(), &RED))
            .map_err(|e| anyhow!("{e}"))?
            .label("Горящие туры")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], RED));
        chart
            .draw_series(hot.iter().map(|&(d, p)| Circle::new((d, p), 4, RED.filled())))
            .map_err(|e| anyhow!("{e}"))?;

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

/// Рисует график «день вылета → минимальная цена» и возвращает PNG.
pub fn render_price_chart(points: &[(NaiveDate, u64)], title: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(!points.is_empty(), "нет точек для графика");

    let path = std::env::temp_dir().join(format!("tv_chart_{}.png", std::process::id()));
    {
        let root = BitMapBackend::new(&path, (1000, 560)).into_drawing_area();
        root.fill(&WHITE).map_err(|e| anyhow!("{e}"))?;

        let min_date = points.first().unwrap().0;
        let max_date = points.last().unwrap().0;
        let min_price = points.iter().map(|p| p.1).min().unwrap();
        let max_price = points.iter().map(|p| p.1).max().unwrap();
        // Запас сверху и снизу, чтобы линия не липла к краям.
        let pad = ((max_price - min_price) / 10).max(2000);
        let y_lo = min_price.saturating_sub(pad);
        let y_hi = max_price + pad;

        let days = (max_date - min_date).num_days().max(1) as usize + 1;
        let mut chart = ChartBuilder::on(&root)
            .margin(24)
            .caption(title, ("sans-serif", 24))
            .x_label_area_size(58)
            .y_label_area_size(80)
            .build_cartesian_2d(min_date..max_date, y_lo..y_hi)
            .map_err(|e| anyhow!("{e}"))?;

        chart
            .configure_mesh()
            .x_labels(days)
            .y_labels(8)
            .x_label_formatter(&|d| d.format("%d.%m").to_string())
            .y_label_formatter(&|p| format!("{} тыс.", p / 1000))
            .label_style(("sans-serif", 15))
            .draw()
            .map_err(|e| anyhow!("{e}"))?;

        chart
            .draw_series(LineSeries::new(points.iter().map(|&(d, p)| (d, p)), &BLUE))
            .map_err(|e| anyhow!("{e}"))?;
        chart
            .draw_series(
                points
                    .iter()
                    .map(|&(d, p)| Circle::new((d, p), 4, BLUE.filled())),
            )
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

    #[test]
    fn renders_png() {
        let points: Vec<(NaiveDate, u64)> = (1..=28)
            .map(|d| {
                (
                    NaiveDate::from_ymd_opt(2026, 7, d).unwrap(),
                    150_000 + (d as u64 * 13 % 7) * 8_000,
                )
            })
            .collect();
        let png = render_price_chart(&points, "Тестовый график (2 взр., 8–11 ночей)").unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::write("/tmp/test_chart.png", &png).unwrap();
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn renders_overlay_png() {
        let regular: Vec<(NaiveDate, u64)> = (1..=28)
            .map(|d| (NaiveDate::from_ymd_opt(2026, 7, d).unwrap(), 140_000 + (d as u64 * 7 % 9) * 9_000))
            .collect();
        let hot: Vec<(NaiveDate, u64)> = [3u32, 6, 9, 14, 20]
            .iter()
            .map(|&d| (NaiveDate::from_ymd_opt(2026, 7, d).unwrap(), 95_000 + (d as u64) * 2_000))
            .collect();
        let png = render_overlay_chart(&regular, &hot, "Обычные туры до 300 тыс. ₽ vs горящие").unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::write("/tmp/test_overlay.png", &png).unwrap();
    }
}
