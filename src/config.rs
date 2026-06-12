use anyhow::{Context, Result};

pub struct Config {
    /// Токен сообщества ВК (с правами messages)
    pub vk_token: String,
    /// id получателей рассылки (user id или peer id), через запятую
    pub vk_peer_ids: Vec<i64>,
    /// Путь к файлу SQLite
    pub db_path: String,
    /// Время ежедневного запуска, "ЧЧ:ММ" локального времени
    pub run_at: (u32, u32),
    /// Код города вылета в tourvisor (Екатеринбург = 3)
    pub tv_city: u32,
    /// Код страны в tourvisor (Вьетнам = 16)
    pub tv_country: u32,
    /// Окно вылета: строго больше WINDOW_FROM_DAYS и строго меньше WINDOW_TO_DAYS дней от сегодня
    pub window_days: (i64, i64),
    /// Количество взрослых (для поиска цен по дням)
    pub adults: u32,
    /// Длительность тура в ночах, включительно
    pub nights: (u32, u32),
    /// Потолок цены для «обычных» туров на втором графике, ₽
    pub price_limit: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let vk_token = std::env::var("VK_TOKEN").context("не задан VK_TOKEN")?;
        // Пусто — значит без ежедневной рассылки, бот отвечает только по кнопке.
        let vk_peer_ids = std::env::var("VK_PEER_IDS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse::<i64>().context("VK_PEER_IDS: ожидаются числа"))
            .collect::<Result<Vec<_>>>()?;

        let run_at_raw = std::env::var("RUN_AT").unwrap_or_else(|_| "09:00".into());
        let (h, m) = run_at_raw
            .split_once(':')
            .context("RUN_AT: ожидается формат ЧЧ:ММ")?;
        let run_at = (
            h.parse().context("RUN_AT: неверный час")?,
            m.parse().context("RUN_AT: неверные минуты")?,
        );

        Ok(Self {
            vk_token,
            vk_peer_ids,
            db_path: std::env::var("DB_PATH").unwrap_or_else(|_| "tours.db".into()),
            run_at,
            tv_city: std::env::var("TV_CITY")
                .unwrap_or_else(|_| "3".into())
                .parse()
                .context("TV_CITY: ожидается число")?,
            tv_country: std::env::var("TV_COUNTRY")
                .unwrap_or_else(|_| "16".into())
                .parse()
                .context("TV_COUNTRY: ожидается число")?,
            window_days: (
                std::env::var("WINDOW_FROM_DAYS")
                    .unwrap_or_else(|_| "5".into())
                    .parse()
                    .context("WINDOW_FROM_DAYS: ожидается число")?,
                std::env::var("WINDOW_TO_DAYS")
                    .unwrap_or_else(|_| "12".into())
                    .parse()
                    .context("WINDOW_TO_DAYS: ожидается число")?,
            ),
            adults: std::env::var("ADULTS")
                .unwrap_or_else(|_| "2".into())
                .parse()
                .context("ADULTS: ожидается число")?,
            nights: (
                std::env::var("NIGHTS_FROM")
                    .unwrap_or_else(|_| "8".into())
                    .parse()
                    .context("NIGHTS_FROM: ожидается число")?,
                std::env::var("NIGHTS_TO")
                    .unwrap_or_else(|_| "11".into())
                    .parse()
                    .context("NIGHTS_TO: ожидается число")?,
            ),
            price_limit: std::env::var("PRICE_LIMIT")
                .unwrap_or_else(|_| "300000".into())
                .parse()
                .context("PRICE_LIMIT: ожидается число")?,
        })
    }
}
