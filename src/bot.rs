use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::Local;
use serde_json::Value;

use crate::config::Config;
use crate::db::Db;
use crate::tourvisor;
use crate::chart::{Series, COLOR_HOT, COLOR_HOT_OLD, COLOR_REGULAR, COLOR_REGULAR_OLD};
use crate::vk::{Vk, CHART_BUTTON, RETRO_BUTTON, SHOW_BUTTON, TOURS_BUTTON};

/// Слушает Bots Long Poll и отвечает на сообщения:
/// кнопка «Запросить туры» запускает сбор, всё остальное — «не понимаю».
pub async fn longpoll_loop(
    cfg: Arc<Config>,
    db: Arc<Mutex<Db>>,
    client: reqwest::Client,
    vk: Arc<Vk>,
) -> Result<()> {
    let group_id = group_id(&vk).await?;

    let today = Local::now().date_naive();
    vk.set_show_tours(db.lock().unwrap().fetched_on(today)?);

    // Включаем Long Poll и событие message_new, чтобы не настраивать руками.
    if let Err(e) = vk
        .call(
            "groups.setLongPollSettings",
            &[
                ("group_id", group_id.to_string()),
                ("enabled", "1".into()),
                ("message_new", "1".into()),
                ("api_version", "5.199".into()),
            ],
        )
        .await
    {
        eprintln!("groups.setLongPollSettings: {e:#} (включи Long Poll в настройках сообщества вручную)");
    }

    let (mut server, mut key, mut ts) = longpoll_server(&vk, group_id).await?;
    println!("long poll запущен (group {group_id})");

    loop {
        let resp: Value = match client
            .get(&server)
            .query(&[("act", "a_check"), ("key", &key), ("ts", &ts), ("wait", "25")])
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(r) => match r.json().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("long poll: не JSON: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            },
            Err(e) => {
                eprintln!("long poll: {e:#}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        match resp["failed"].as_i64() {
            Some(1) => {
                ts = resp["ts"].as_str().unwrap_or(&ts).to_string();
                continue;
            }
            Some(_) => {
                (server, key, ts) = longpoll_server(&vk, group_id).await?;
                continue;
            }
            None => {}
        }

        if let Some(new_ts) = resp["ts"].as_str() {
            ts = new_ts.to_string();
        }

        for update in resp["updates"].as_array().cloned().unwrap_or_default() {
            if update["type"] != "message_new" {
                continue;
            }
            let msg = &update["object"]["message"];
            let Some(peer_id) = msg["peer_id"].as_i64() else { continue };
            let text = msg["text"].as_str().unwrap_or_default();
            let payload = msg["payload"].as_str().unwrap_or_default();

            if let Err(e) = handle_message(&cfg, &db, &client, &vk, peer_id, text, payload).await {
                eprintln!("обработка сообщения peer_id={peer_id}: {e:#}");
            }
        }
    }
}

async fn handle_message(
    cfg: &Config,
    db: &Mutex<Db>,
    client: &reqwest::Client,
    vk: &Vk,
    peer_id: i64,
    text: &str,
    payload: &str,
) -> Result<()> {
    let today = Local::now().date_naive();
    // Кнопка «Показать туры» доступна, только если сегодня уже собирали.
    vk.set_show_tours(db.lock().unwrap().fetched_on(today)?);

    // Подпись кнопки может содержать суффикс «✅» — сравниваем по началу.
    let asked_fetch = payload.contains("\"cmd\":\"tours\"")
        || text.trim().to_lowercase().starts_with(&TOURS_BUTTON.to_lowercase())
        || text.trim().to_lowercase() == "туры";
    let asked_show = payload.contains("\"cmd\":\"show\"")
        || text.trim().eq_ignore_ascii_case(SHOW_BUTTON);
    // RETRO_BUTTON начинается со слова, похожего на CHART_BUTTON — проверяем ретро первым.
    let asked_retro = payload.contains("\"cmd\":\"retro\"")
        || text.trim().eq_ignore_ascii_case(RETRO_BUTTON)
        || text.trim().to_lowercase() == "ретро";
    let asked_chart = !asked_retro
        && (payload.contains("\"cmd\":\"chart\"")
            || text.trim().eq_ignore_ascii_case(CHART_BUTTON)
            || text.trim().to_lowercase() == "график");

    if asked_chart {
        // Только данные из базы — на tourvisor по кнопкам не ходим.
        if let Err(e) = send_chart(cfg, db, vk, peer_id).await {
            eprintln!("график не получился: {e:#}");
            vk.send_text(
                peer_id,
                &format!("В базе ещё нет цен — сначала нажми «{TOURS_BUTTON}»."),
            )
            .await?;
        }
        return Ok(());
    }

    if asked_retro {
        if let Err(e) = send_retro_chart(db, vk, peer_id).await {
            eprintln!("ретроспективный график не получился: {e:#}");
            vk.send_text(
                peer_id,
                &format!("Для ретроспективы пока мало данных — собери их кнопкой «{TOURS_BUTTON}»."),
            )
            .await?;
        }
        return Ok(());
    }

    if asked_show {
        let tours = db
            .lock()
            .unwrap()
            .load_relevant(today, cfg.window_days, cfg.nights)?;
        if tours.is_empty() {
            vk.send_text(
                peer_id,
                &format!("В базе нет актуальных туров — нажми «{TOURS_BUTTON}»."),
            )
            .await?;
        } else {
            vk.send_text(
                peer_id,
                &format!("💾 Показываю актуальные туры из базы: {}.", tours.len()),
            )
            .await?;
            vk.send_digest(peer_id, &tours).await?;
        }
        return Ok(());
    }

    if !asked_fetch {
        vk.send_text(
            peer_id,
            &format!("Не понимаю 🤷 Нажми кнопку «{TOURS_BUTTON}» внизу — пришлю свежие горящие туры."),
        )
        .await?;
        return Ok(());
    }

    // «Запросить туры» только собирает данные в базу. Карточки — по кнопке
    // «Показать туры», графики — по «График цен» и «Средние цены».
    if db.lock().unwrap().fetched_on(today)? {
        vk.send_text(
            peer_id,
            &format!(
                "💾 Сегодня уже парсили, данные в базе. Карточки — «{SHOW_BUTTON}», \
                 графики — «{CHART_BUTTON}» и «{RETRO_BUTTON}»."
            ),
        )
        .await?;
        return Ok(());
    }

    vk.send_text(
        peer_id,
        &format!(
            "🔍 Запускаю дневную сессию парсинга: горящие туры и цены на 1,5 месяца \
             ({} взр., {}–{} ночей), это полминуты…",
            cfg.adults, cfg.nights.0, cfg.nights.1
        ),
    )
    .await?;
    let stats = parse_session(cfg, db, client).await.context("сессия парсинга")?;
    vk.set_show_tours(true);
    vk.send_text(
        peer_id,
        &format!(
            "✅ Готово. Всего горящих туров: {}, в окне вылета ({}–{} дн.): {}. \
             Новых в БД: {}, обновлено: {}. Цены по дням за 1,5 месяца сохранены.\n\
             Карточки — «{SHOW_BUTTON}», графики — «{CHART_BUTTON}» и «{RETRO_BUTTON}».",
            stats.total,
            cfg.window_days.0,
            cfg.window_days.1,
            stats.in_window,
            stats.inserted,
            stats.updated,
        ),
    )
    .await?;
    Ok(())
}

/// Итоги дневной сессии парсинга.
pub struct ParseStats {
    pub total: usize,
    pub in_window: usize,
    pub inserted: usize,
    pub updated: usize,
}

/// Единственная сессия парсинга за день: горящие туры (modhot)
/// и цены по дням на 1,5 месяца (настоящий поиск). Всё сохраняется в БД,
/// дальше кнопки и графики работают только с базой.
pub async fn parse_session(
    cfg: &Config,
    db: &Mutex<Db>,
    client: &reqwest::Client,
) -> Result<ParseStats> {
    let today = Local::now().date_naive();

    // modhot параметр regions игнорирует — горящие фильтруем по коду региона сами.
    let all: Vec<_> = tourvisor::fetch_hot_tours(client, cfg.tv_city, cfg.tv_country)
        .await?
        .into_iter()
        .filter(|t| cfg.tv_regions.is_empty() || cfg.tv_regions.contains(&t.region_code))
        .collect();
    let mut tours: Vec<_> = all
        .iter()
        .filter(|t| {
            tourvisor::in_window(t, today, cfg.window_days)
                && (cfg.nights.0..=cfg.nights.1).contains(&t.nights)
        })
        .cloned()
        .collect();
    tours.sort_by_key(|t| t.price);
    let (inserted, updated) = db.lock().unwrap().save_snapshot(&tours, today)?;

    // Горящие туры не должны попадать в статистику «обычных».
    let hot_ids: std::collections::HashSet<String> =
        all.iter().map(|t| t.tour_id.clone()).collect();
    let prices = tourvisor::fetch_month_prices(
        client,
        cfg.tv_city,
        cfg.tv_country,
        cfg.adults,
        cfg.nights,
        cfg.price_limit,
        &cfg.tv_regions,
        &hot_ids,
        today,
    )
    .await?;
    anyhow::ensure!(!prices.min_all.is_empty(), "поиск не вернул цен по дням");

    let capped_min: std::collections::BTreeMap<_, _> = prices.min_capped.iter().copied().collect();
    let capped_avg: std::collections::BTreeMap<_, _> = prices.avg_capped.iter().copied().collect();
    let day_rows: Vec<crate::db::DayPrice> = prices
        .min_all
        .iter()
        .map(|&(d, min_all)| crate::db::DayPrice {
            fly_date: d,
            min_all,
            min_capped: capped_min.get(&d).copied().unwrap_or(0),
            avg_capped: capped_avg.get(&d).copied().unwrap_or(0),
        })
        .collect();
    db.lock().unwrap().save_day_prices(&day_rows, today)?;

    Ok(ParseStats {
        total: all.len(),
        in_window: tours.len(),
        inserted,
        updated,
    })
}

/// Один график из БД: минимальные цены по дням вылета,
/// обычные туры (до лимита) и горящие подписаны отдельно. На tourvisor не ходит.
pub async fn send_chart(cfg: &Config, db: &Mutex<Db>, vk: &Vk, peer_id: i64) -> Result<()> {
    let today = Local::now().date_naive();
    let (cached, hot_tours) = {
        let db = db.lock().unwrap();
        (db.latest_day_prices()?, db.load_upcoming(today)?)
    };
    let (fetched, rows) = cached.context("в БД нет цен по дням")?;
    anyhow::ensure!(!rows.is_empty(), "в БД нет цен по дням");

    let regular: Vec<_> = rows
        .iter()
        .filter(|r| r.min_capped > 0)
        .map(|r| (r.fly_date, r.min_capped))
        .collect();
    let hot = tourvisor::hot_day_prices(&hot_tours);
    let stale_note = if fetched == today {
        String::new()
    } else {
        format!(" (данные за {})", fetched.format("%d.%m"))
    };

    let series = [
        Series {
            label: format!("Обычные туры (до {} тыс. ₽)", cfg.price_limit / 1000),
            color: COLOR_REGULAR,
            points: regular,
        },
        Series {
            label: "Горящие туры".into(),
            color: COLOR_HOT,
            points: hot,
        },
    ];
    let title = format!(
        "Мин. цены по дням вылета ({} взр., {}–{} ночей)",
        cfg.adults, cfg.nights.0, cfg.nights.1
    );
    let png = crate::chart::render_chart(&title, &series)?;
    vk.send_photo(
        peer_id,
        png,
        &format!(
            "📊 Цены на 1,5 месяца вперёд{stale_note}: синим — обычные туры, красным — горящие."
        ),
    )
    .await
}

/// Ретроспективный график из БД: окно «сейчас − месяц … сейчас + месяц»,
/// старые и новые цены обычных и горящих туров. На tourvisor не ходит.
pub async fn send_retro_chart(db: &Mutex<Db>, vk: &Vk, peer_id: i64) -> Result<()> {
    let today = Local::now().date_naive();
    let from = today - chrono::Duration::days(30);
    let to = today + chrono::Duration::days(30);

    let (history, hot_tours) = {
        let db = db.lock().unwrap();
        (
            db.day_price_history(from, to)?,
            db.load_relevant(today, (-31, 31), (0, 99))?,
        )
    };

    // Обычные: минимум из первого снимка («было») и из последнего («сейчас»).
    let regular_old: Vec<_> = history.iter().map(|&(d, first, _)| (d, first)).collect();
    let regular_new: Vec<_> = history.iter().map(|&(d, _, last)| (d, last)).collect();

    // Горящие: по каждому дню — самый дешёвый тур, его цена и его старая цена.
    let mut cheapest: std::collections::BTreeMap<chrono::NaiveDate, (u64, u64)> = Default::default();
    for t in &hot_tours {
        let entry = cheapest.entry(t.fly_date).or_insert((t.price, t.price_old));
        if t.price < entry.0 {
            *entry = (t.price, t.price_old);
        }
    }
    let hot_new: Vec<_> = cheapest.iter().map(|(&d, &(p, _))| (d, p)).collect();
    let hot_old: Vec<_> = cheapest
        .iter()
        .filter(|(_, &(_, old))| old > 0)
        .map(|(&d, &(_, old))| (d, old))
        .collect();

    anyhow::ensure!(
        !regular_new.is_empty() || !hot_new.is_empty(),
        "в БД нет данных за этот период"
    );

    let series = [
        Series {
            label: "Обычные: было".into(),
            color: COLOR_REGULAR_OLD,
            points: regular_old,
        },
        Series {
            label: "Обычные: сейчас".into(),
            color: COLOR_REGULAR,
            points: regular_new,
        },
        Series {
            label: "Горящие: было".into(),
            color: COLOR_HOT_OLD,
            points: hot_old,
        },
        Series {
            label: "Горящие: сейчас".into(),
            color: COLOR_HOT,
            points: hot_new,
        },
    ];
    let title = format!(
        "Ретроспектива цен: {} — {}",
        from.format("%d.%m"),
        to.format("%d.%m")
    );
    let png = crate::chart::render_chart(&title, &series)?;
    vk.send_photo(
        peer_id,
        png,
        "📜 Ретроспектива за ±месяц (только данные из базы): светлым — старые цены, ярким — текущие; синие — обычные туры, красные — горящие.",
    )
    .await
}

async fn group_id(vk: &Vk) -> Result<i64> {
    let resp = vk.call("groups.getById", &[]).await?;
    // v5.199 отдаёт {"groups":[...]}, старые версии — массив сразу.
    resp["groups"][0]["id"]
        .as_i64()
        .or_else(|| resp[0]["id"].as_i64())
        .context("groups.getById: не нашёл id сообщества (токен не от сообщества?)")
}

async fn longpoll_server(vk: &Vk, group_id: i64) -> Result<(String, String, String)> {
    let resp = vk
        .call("groups.getLongPollServer", &[("group_id", group_id.to_string())])
        .await?;
    Ok((
        resp["server"].as_str().context("нет server")?.to_string(),
        resp["key"].as_str().context("нет key")?.to_string(),
        resp["ts"].as_str().context("нет ts")?.to_string(),
    ))
}
