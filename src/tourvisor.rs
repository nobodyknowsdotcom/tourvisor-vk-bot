use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate};
use serde::Deserialize;

/// Неофициальный эндпоинт виджета «горящие туры» tourvisor.ru.
/// Параметры city/countries подсмотрены в module/v6.x — может поменяться без предупреждения.
const HOT_URL: &str = "https://tourvisor.ru/xml/modhot.php";

#[derive(Debug, Deserialize)]
struct HotResponse {
    #[serde(default)]
    hot: Vec<RawHotTour>,
}

#[derive(Debug, Deserialize)]
struct RawHotTour {
    price: String,
    priceold: String,
    tourid2: String,
    countryname: String,
    departure: String,
    hotelcode: String,
    hotelname: String,
    hotelstars: String,
    hotelregionname: String,
    #[serde(default)]
    hotelregioncode: String,
    #[serde(default)]
    hotelrating: String,
    #[serde(default)]
    hotelpicture: String,
    flydate: String,
    nights: String,
    #[serde(default)]
    meal: String,
}

#[derive(Debug, Clone)]
pub struct HotTour {
    pub tour_id: String,
    pub country: String,
    pub departure: String,
    pub hotel_code: String,
    pub hotel_name: String,
    pub hotel_stars: u8,
    pub region: String,
    /// Код региона tourvisor (87 = Нячанг); 0, если не разобрался
    pub region_code: u32,
    pub rating: String,
    pub picture_url: String,
    /// Ссылка на поиск по этому отелю на tourvisor (для кликабельной карточки)
    pub hotel_url: String,
    pub price: u64,
    pub price_old: u64,
    pub nights: u32,
    pub fly_date: NaiveDate,
    pub return_date: NaiveDate,
    pub meal: String,
}

pub async fn fetch_hot_tours(client: &reqwest::Client, city: u32, country: u32) -> Result<Vec<HotTour>> {
    let resp: HotResponse = client
        .get(HOT_URL)
        .query(&[
            ("format", "json".to_string()),
            ("city", city.to_string()),
            ("countries", country.to_string()),
        ])
        .header("Referer", "https://tourvisor.ru/")
        .send()
        .await
        .context("запрос к tourvisor не прошёл")?
        .error_for_status()?
        .json()
        .await
        .context("tourvisor вернул не тот JSON, что ожидался (возможно, формат сменился)")?;

    resp.hot
        .into_iter()
        .map(|raw| parse_tour(raw, city, country))
        .collect()
}

fn parse_tour(raw: RawHotTour, city: u32, country: u32) -> Result<HotTour> {
    let fly_date = NaiveDate::parse_from_str(&raw.flydate, "%d.%m.%Y")
        .with_context(|| format!("не разобрал дату вылета: {}", raw.flydate))?;
    let nights: u32 = raw.nights.parse().context("nights: не число")?;
    // modhot отдаёт цену за одного человека (страница поиска по ссылке
    // показывает за двоих, ровно вдвое больше) — приводим к цене за двоих.
    let price: u64 = raw.price.parse().context("price: не число")?;
    let price_old: u64 = raw.priceold.parse().unwrap_or(0);
    // Ссылка ведёт на конкретный тур: модуль поиска открывает его по якорю tvtourid.
    let hotel_url = format!(
        "https://tourvisor.ru/search.php?departure={city}&country={country}&hotels={}#tvtourid={}",
        raw.hotelcode, raw.tourid2
    );

    Ok(HotTour {
        tour_id: raw.tourid2,
        country: raw.countryname,
        departure: raw.departure,
        hotel_code: raw.hotelcode,
        hotel_name: raw.hotelname,
        hotel_stars: raw.hotelstars.parse().unwrap_or(0),
        region: raw.hotelregionname,
        region_code: raw.hotelregioncode.parse().unwrap_or(0),
        rating: raw.hotelrating,
        picture_url: raw.hotelpicture,
        hotel_url,
        price: price * 2,
        price_old: price_old * 2,
        nights,
        fly_date,
        return_date: fly_date + Duration::days(nights as i64),
        meal: raw.meal,
    })
}

/// Вылет строго позже, чем через `from_days`, и раньше, чем через `to_days` дней.
pub fn in_window(tour: &HotTour, today: NaiveDate, (from_days, to_days): (i64, i64)) -> bool {
    let from = today + Duration::days(from_days);
    let to = today + Duration::days(to_days);
    tour.fly_date > from && tour.fly_date < to
}

/// Эндпоинты реального поиска tourvisor: запуск и опрос результатов.
const SEARCH_URL: &str = "https://tourvisor.ru/xml/modsearch.php";
const RESULT_URL: &str = "https://search3.tourvisor.ru/modresult.php";
/// Горизонт поиска обычных туров — полтора месяца.
pub const SEARCH_DAYS: i64 = 45;

/// Цены обычных туров по дням вылета за месяц.
pub struct MonthPrices {
    /// Минимальная цена за день среди всех туров
    pub min_all: Vec<(NaiveDate, u64)>,
    /// Минимальная цена за день среди туров не дороже лимита
    pub min_capped: Vec<(NaiveDate, u64)>,
    /// Средняя цена за день среди туров не дороже лимита
    pub avg_capped: Vec<(NaiveDate, u64)>,
}

/// Цены обычных туров по дням вылета на ближайшие полтора месяца
/// (настоящий поиск: учитывает количество взрослых и диапазон ночей).
/// Туры с id из `exclude_ids` (горящие) в статистику не попадают.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_month_prices(
    client: &reqwest::Client,
    city: u32,
    country: u32,
    adults: u32,
    (nights_from, nights_to): (u32, u32),
    price_limit: u64,
    regions: &[u32],
    exclude_ids: &std::collections::HashSet<String>,
    today: NaiveDate,
) -> Result<MonthPrices> {
    let mut query = vec![
        ("format", "json".to_string()),
        ("departure", city.to_string()),
        ("country", country.to_string()),
        ("datefrom", (today + Duration::days(1)).format("%d.%m.%Y").to_string()),
        ("dateto", (today + Duration::days(SEARCH_DAYS)).format("%d.%m.%Y").to_string()),
        ("nightsfrom", nights_from.to_string()),
        ("nightsto", nights_to.to_string()),
        ("adults", adults.to_string()),
        ("child", "0".to_string()),
    ];
    if !regions.is_empty() {
        let joined = regions.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        query.push(("regions", joined));
    }

    let started: serde_json::Value = client
        .get(SEARCH_URL)
        .query(&query)
        .header("Referer", "https://tourvisor.ru/")
        .send()
        .await
        .context("modsearch: запрос не прошёл")?
        .error_for_status()?
        .json()
        .await
        .context("modsearch: не JSON")?;

    let request_id = started["result"]["requestid"]
        .as_u64()
        .context("modsearch: нет requestid")?;

    // Поиск идёт по операторам постепенно — опрашиваем, пока progress < 100.
    let mut pairs: Vec<(NaiveDate, u64)> = Vec::new();
    for attempt in 1..=12 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let resp: serde_json::Value = client
            .get(RESULT_URL)
            .query(&[
                ("format", "json".to_string()),
                ("requestid", request_id.to_string()),
                ("type", "result".to_string()),
                ("page", "1".to_string()),
                ("onpage", "1000".to_string()),
            ])
            .header("Referer", "https://tourvisor.ru/")
            .send()
            .await
            .context("modresult: запрос не прошёл")?
            .error_for_status()?
            .json()
            .await
            .context("modresult: не JSON")?;

        pairs.clear();
        collect_price_pairs(&resp["data"]["block"], exclude_ids, &mut pairs);

        let progress: u32 = resp["data"]["status"]["progress"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| resp["data"]["status"]["progress"].as_u64().map(|n| n as u32))
            .unwrap_or(0);
        println!("поиск цен по дням: {progress}% (попытка {attempt})");
        if progress >= 100 {
            break;
        }
    }

    let capped: Vec<(NaiveDate, u64)> =
        pairs.iter().copied().filter(|&(_, p)| p <= price_limit).collect();
    Ok(MonthPrices {
        min_all: day_min(&pairs),
        min_capped: day_min(&capped),
        avg_capped: day_avg(&capped),
    })
}

/// Минимальная цена горящих туров на каждый день вылета (для графика-наложения).
pub fn hot_day_prices(tours: &[HotTour]) -> Vec<(NaiveDate, u64)> {
    day_min(&tours.iter().map(|t| (t.fly_date, t.price)).collect::<Vec<_>>())
}

fn day_min(pairs: &[(NaiveDate, u64)]) -> Vec<(NaiveDate, u64)> {
    let mut best: std::collections::BTreeMap<NaiveDate, u64> = Default::default();
    for &(date, price) in pairs {
        let entry = best.entry(date).or_insert(u64::MAX);
        *entry = (*entry).min(price);
    }
    best.into_iter().collect()
}

fn day_avg(pairs: &[(NaiveDate, u64)]) -> Vec<(NaiveDate, u64)> {
    let mut acc: std::collections::BTreeMap<NaiveDate, (u64, u64)> = Default::default();
    for &(date, price) in pairs {
        let entry = acc.entry(date).or_insert((0, 0));
        entry.0 += price;
        entry.1 += 1;
    }
    acc.into_iter().map(|(d, (sum, n))| (d, sum / n)).collect()
}

/// Рекурсивно собирает пары (dt, pr) из блоков результата поиска,
/// пропуская туры из `exclude_ids` (горящие).
fn collect_price_pairs(
    node: &serde_json::Value,
    exclude_ids: &std::collections::HashSet<String>,
    pairs: &mut Vec<(NaiveDate, u64)>,
) {
    match node {
        serde_json::Value::Object(map) => {
            if let (Some(dt), Some(pr)) = (
                map.get("dt").and_then(|v| v.as_str()),
                map.get("pr").and_then(|v| v.as_u64()),
            ) {
                let excluded = map
                    .get("id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| exclude_ids.contains(id));
                if !excluded {
                    if let Ok(date) = NaiveDate::parse_from_str(dt, "%Y-%m-%d") {
                        pairs.push((date, pr));
                    }
                }
            }
            for v in map.values() {
                collect_price_pairs(v, exclude_ids, pairs);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_price_pairs(v, exclude_ids, pairs);
            }
        }
        _ => {}
    }
}
