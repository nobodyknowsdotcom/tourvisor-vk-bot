use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection};

use crate::tourvisor::HotTour;

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS hot_tours (
                id          INTEGER PRIMARY KEY,
                tour_id     TEXT NOT NULL,
                fetched_on  TEXT NOT NULL,
                country     TEXT NOT NULL,
                departure   TEXT NOT NULL,
                hotel_code  TEXT NOT NULL,
                hotel_name  TEXT NOT NULL,
                hotel_stars INTEGER NOT NULL,
                region      TEXT NOT NULL,
                rating      TEXT NOT NULL,
                picture_url TEXT NOT NULL,
                hotel_url   TEXT NOT NULL,
                price       INTEGER NOT NULL,
                price_old   INTEGER NOT NULL,
                nights      INTEGER NOT NULL,
                fly_date    TEXT NOT NULL,
                return_date TEXT NOT NULL,
                meal        TEXT NOT NULL,
                UNIQUE (tour_id, fetched_on)
            );
            CREATE TABLE IF NOT EXISTS regular_day_prices (
                id         INTEGER PRIMARY KEY,
                fetched_on TEXT NOT NULL,
                fly_date   TEXT NOT NULL,
                min_price  INTEGER NOT NULL,
                UNIQUE (fetched_on, fly_date)
            );
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // Миграции для старых баз: средняя цена и минимум без лимита.
        let _ = conn.execute(
            "ALTER TABLE regular_day_prices ADD COLUMN avg_price INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE regular_day_prices ADD COLUMN min_all INTEGER NOT NULL DEFAULT 0",
            [],
        );
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_hot_tours_tour_id ON hot_tours (tour_id)",
            [],
        )?;
        Ok(Self { conn })
    }

    /// Сохраняет снимок туров: новые добавляются, уже известные (тот же tour_id)
    /// обновляются, если их данные изменились. Возвращает (новых, обновлённых).
    pub fn save_snapshot(&self, tours: &[HotTour], fetched_on: NaiveDate) -> Result<(usize, usize)> {
        let tx = self.conn.unchecked_transaction()?;
        let (mut inserted, mut updated) = (0, 0);
        {
            let mut exists_stmt = tx.prepare("SELECT 1 FROM hot_tours WHERE tour_id = ?1")?;
            let mut upsert_stmt = tx.prepare(
                "INSERT INTO hot_tours
                 (tour_id, fetched_on, country, departure, hotel_code, hotel_name, hotel_stars,
                  region, rating, picture_url, hotel_url, price, price_old, nights, fly_date, return_date, meal)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                 ON CONFLICT (tour_id) DO UPDATE SET
                    price = excluded.price,
                    price_old = excluded.price_old,
                    nights = excluded.nights,
                    fly_date = excluded.fly_date,
                    return_date = excluded.return_date,
                    meal = excluded.meal,
                    rating = excluded.rating,
                    picture_url = excluded.picture_url,
                    hotel_url = excluded.hotel_url
                 WHERE price != excluded.price
                    OR price_old != excluded.price_old
                    OR fly_date != excluded.fly_date
                    OR nights != excluded.nights
                    OR hotel_url != excluded.hotel_url",
            )?;
            for t in tours {
                let existed = exists_stmt.exists(params![t.tour_id])?;
                let changed = upsert_stmt.execute(params![
                    t.tour_id,
                    fetched_on.to_string(),
                    t.country,
                    t.departure,
                    t.hotel_code,
                    t.hotel_name,
                    t.hotel_stars,
                    t.region,
                    t.rating,
                    t.picture_url,
                    t.hotel_url,
                    t.price,
                    t.price_old,
                    t.nights,
                    t.fly_date.to_string(),
                    t.return_date.to_string(),
                    t.meal,
                ])?;
                match (existed, changed) {
                    (false, _) => inserted += 1,
                    (true, n) if n > 0 => updated += 1,
                    _ => {}
                }
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_fetch_date', ?1)",
            params![fetched_on.to_string()],
        )?;
        tx.commit()?;
        Ok((inserted, updated))
    }

    /// Собирали ли туры в указанный день (дата последнего парсинга).
    pub fn fetched_on(&self, day: NaiveDate) -> Result<bool> {
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_fetch_date'",
                [],
                |row| row.get(0),
            )
            .ok();
        Ok(last.as_deref() == Some(day.to_string().as_str()))
    }

    /// Актуальные туры из базы: вылет в заданном окне от `today`,
    /// ночи в диапазоне. Отсортированы по цене.
    pub fn load_relevant(
        &self,
        today: NaiveDate,
        (from_days, to_days): (i64, i64),
        (nights_from, nights_to): (u32, u32),
    ) -> Result<Vec<HotTour>> {
        let from = (today + chrono::Duration::days(from_days)).to_string();
        let to = (today + chrono::Duration::days(to_days)).to_string();
        let mut stmt = self.conn.prepare(
            "SELECT tour_id, country, departure, hotel_code, hotel_name, hotel_stars,
                    region, rating, picture_url, hotel_url, price, price_old, nights, fly_date, return_date, meal
             FROM hot_tours
             WHERE fly_date > ?1 AND fly_date < ?2 AND nights BETWEEN ?3 AND ?4
               AND id IN (SELECT MIN(id) FROM hot_tours GROUP BY tour_id)
             ORDER BY price",
        )?;
        let tours = stmt
            .query_map(params![from, to, nights_from, nights_to], |row| {
                Ok(HotTour {
                    tour_id: row.get(0)?,
                    country: row.get(1)?,
                    departure: row.get(2)?,
                    hotel_code: row.get(3)?,
                    hotel_name: row.get(4)?,
                    hotel_stars: row.get(5)?,
                    region: row.get(6)?,
                    rating: row.get(7)?,
                    picture_url: row.get(8)?,
                    hotel_url: row.get(9)?,
                    price: row.get(10)?,
                    price_old: row.get(11)?,
                    nights: row.get(12)?,
                    fly_date: row.get::<_, String>(13)?.parse().expect("дата в БД"),
                    return_date: row.get::<_, String>(14)?.parse().expect("дата в БД"),
                    meal: row.get(15)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tours)
    }

    /// Горящие туры из базы с вылетом начиная с `today` (для графика средних).
    pub fn load_upcoming(&self, today: NaiveDate) -> Result<Vec<HotTour>> {
        self.load_relevant(today - chrono::Duration::days(1), (0, 365), (0, 99))
    }

    /// Дневной снимок цен обычных туров:
    /// (день вылета, минимум среди всех, минимум до лимита, среднее до лимита).
    pub fn save_day_prices(&self, points: &[DayPrice], fetched_on: NaiveDate) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO regular_day_prices
                 (fetched_on, fly_date, min_all, min_price, avg_price)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for p in points {
                stmt.execute(params![
                    fetched_on.to_string(),
                    p.fly_date.to_string(),
                    p.min_all,
                    p.min_capped,
                    p.avg_capped
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Последний сохранённый снимок цен по дням и дата его сбора.
    pub fn latest_day_prices(&self) -> Result<Option<(NaiveDate, Vec<DayPrice>)>> {
        let last: Option<String> = self
            .conn
            .query_row("SELECT MAX(fetched_on) FROM regular_day_prices", [], |row| row.get(0))
            .ok()
            .flatten();
        let Some(last) = last else { return Ok(None) };

        let mut stmt = self.conn.prepare(
            "SELECT fly_date, min_all, min_price, avg_price FROM regular_day_prices
             WHERE fetched_on = ?1 ORDER BY fly_date",
        )?;
        let rows = stmt
            .query_map(params![last], |row| {
                Ok(DayPrice {
                    fly_date: row.get::<_, String>(0)?.parse().expect("дата в БД"),
                    min_all: row.get(1)?,
                    min_capped: row.get(2)?,
                    avg_capped: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some((last.parse().expect("дата в БД"), rows)))
    }
}

/// Сводка цен обычных туров за один день вылета.
pub struct DayPrice {
    pub fly_date: NaiveDate,
    pub min_all: u64,
    pub min_capped: u64,
    pub avg_capped: u64,
}
