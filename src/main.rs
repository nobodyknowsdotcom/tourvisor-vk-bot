mod bot;
mod chart;
mod config;
mod db;
mod tourvisor;
mod vk;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{Duration, Local, NaiveTime};
use config::Config;
use db::Db;
use vk::Vk;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Arc::new(Config::from_env()?);
    let db = Arc::new(Mutex::new(Db::open(&cfg.db_path)?));
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()?;
    let vk = Arc::new(Vk::new(client.clone(), cfg.vk_token.clone()));

    if std::env::args().any(|a| a == "--once") {
        return run(&cfg, &db, &client, &vk).await;
    }

    // Заполнение базы при старте (если сегодня ещё не собирали),
    // дальше ежедневная рассылка по расписанию — в фоне,
    // long poll с кнопками — в основной задаче.
    {
        let (cfg, db, client, vk) = (cfg.clone(), db.clone(), client.clone(), vk.clone());
        tokio::spawn(async move {
            let today = Local::now().date_naive();
            let fresh = db.lock().unwrap().fetched_on(today).unwrap_or(false);
            match fresh {
                true => println!("база за сегодня уже заполнена"),
                false => match bot::parse_session(&cfg, &db, &client).await {
                    Ok(stats) => {
                        println!(
                            "стартовое заполнение базы: всего {}, в окне {}, новых {}, обновлено {}",
                            stats.total, stats.in_window, stats.inserted, stats.updated
                        );
                        vk.set_show_tours(true);
                    }
                    Err(e) => eprintln!("стартовое заполнение базы упало: {e:#}"),
                },
            }

            loop {
                let pause = until_next_run(cfg.run_at);
                println!(
                    "следующий плановый запуск через {}ч {}м",
                    pause.num_hours(),
                    pause.num_minutes() % 60
                );
                tokio::time::sleep(pause.to_std().expect("отрицательная пауза")).await;

                if let Err(e) = run(&cfg, &db, &client, &vk).await {
                    eprintln!("ежедневный прогон упал: {e:#}");
                }
            }
        });
    }

    bot::longpoll_loop(cfg, db, client, vk).await
}

async fn run(
    cfg: &Config,
    db: &Mutex<Db>,
    client: &reqwest::Client,
    vk: &Vk,
) -> Result<()> {
    let today = Local::now().date_naive();

    // Единственная сессия парсинга за день: горящие туры + цены на месяц.
    // Если сегодня уже собирали (например, кнопкой), данные переиспользуем.
    if db.lock().unwrap().fetched_on(today)? {
        println!("сегодня уже парсили — рассылаю из базы");
    } else {
        let stats = bot::parse_session(cfg, db, client).await?;
        println!(
            "сессия парсинга: всего {}, в окне {}, новых в БД {}",
            stats.total, stats.in_window, stats.inserted
        );
    }
    vk.set_show_tours(true);

    let tours = db
        .lock()
        .unwrap()
        .load_relevant(today, cfg.window_days, cfg.nights)?;

    for &peer_id in &cfg.vk_peer_ids {
        if let Err(e) = vk.send_digest(peer_id, &tours).await {
            eprintln!("не отправилось peer_id={peer_id}: {e:#}");
        }
        if let Err(e) = bot::send_month_charts(cfg, db, vk, peer_id).await {
            eprintln!("графики для peer_id={peer_id} не отправились: {e:#}");
        }
    }
    Ok(())
}

fn until_next_run((hour, minute): (u32, u32)) -> Duration {
    let now = Local::now();
    let run_time = NaiveTime::from_hms_opt(hour, minute, 0).expect("RUN_AT вне диапазона");
    let mut next = now.date_naive().and_time(run_time);
    if next <= now.naive_local() {
        next += Duration::days(1);
    }
    next - now.naive_local()
}
