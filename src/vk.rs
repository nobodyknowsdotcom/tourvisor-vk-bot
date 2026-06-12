use anyhow::{bail, Context, Result};
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};

use crate::tourvisor::HotTour;

const API: &str = "https://api.vk.com/method";
const API_VERSION: &str = "5.199";
/// Лимит ВК на количество карточек в одной карусели.
const CAROUSEL_LIMIT: usize = 10;
/// Сколько фото грузим в ВК одновременно.
const UPLOAD_CONCURRENCY: usize = 5;

pub struct Vk {
    client: reqwest::Client,
    token: String,
    /// Показывать ли кнопку «Показать туры» (есть ли в БД сегодняшний сбор).
    show_tours: std::sync::atomic::AtomicBool,
}

struct SavedPhoto {
    owner_id: i64,
    id: i64,
    access_key: Option<String>,
}

/// Карусель ВК требует изображения с соотношением сторон 13:8 —
/// фото с другими пропорциями молча не отображаются. Кроп по центру.
fn crop_to_carousel_ratio(bytes: &[u8]) -> Vec<u8> {
    const RATIO_W: u32 = 13;
    const RATIO_H: u32 = 8;

    let Ok(img) = image::load_from_memory(bytes) else {
        return bytes.to_vec();
    };
    let (w, h) = (img.width(), img.height());

    // ВК проверяет пропорцию строго, поэтому стороны должны быть
    // точно кратны 13 и 8: берём наибольший подходящий масштаб.
    let scale = (w / RATIO_W).min(h / RATIO_H);
    if scale == 0 {
        return bytes.to_vec();
    }
    let (crop_w, crop_h) = (RATIO_W * scale, RATIO_H * scale);
    let cropped = img.crop_imm((w - crop_w) / 2, (h - crop_h) / 2, crop_w, crop_h);

    let mut out = std::io::Cursor::new(Vec::new());
    match cropped.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg) {
        Ok(()) => out.into_inner(),
        Err(_) => bytes.to_vec(),
    }
}

pub const TOURS_BUTTON: &str = "Запросить туры";
pub const SHOW_BUTTON: &str = "Показать туры";
pub const CHART_BUTTON: &str = "График";
pub const RETRO_BUTTON: &str = "Ретроспективный график";

impl Vk {
    pub fn new(client: reqwest::Client, token: String) -> Self {
        Self {
            client,
            token,
            show_tours: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Включает/выключает кнопку «Показать туры» в клавиатуре.
    pub fn set_show_tours(&self, show: bool) {
        self.show_tours.store(show, std::sync::atomic::Ordering::Relaxed);
    }

    fn keyboard(&self) -> String {
        let fetched_today = self.show_tours.load(std::sync::atomic::Ordering::Relaxed);
        // В подписи кнопки видно, что сегодняшние данные уже собраны.
        let tours_label = if fetched_today {
            format!("{TOURS_BUTTON} ✅")
        } else {
            TOURS_BUTTON.to_string()
        };
        let mut row = vec![json!({
            "action": { "type": "text", "label": tours_label, "payload": "{\"cmd\":\"tours\"}" },
            "color": "primary"
        })];
        if fetched_today {
            row.push(json!({
                "action": { "type": "text", "label": SHOW_BUTTON, "payload": "{\"cmd\":\"show\"}" },
                "color": "secondary"
            }));
        }
        let charts_row = vec![
            json!({
                "action": { "type": "text", "label": CHART_BUTTON, "payload": "{\"cmd\":\"chart\"}" },
                "color": "secondary"
            }),
            json!({
                "action": { "type": "text", "label": RETRO_BUTTON, "payload": "{\"cmd\":\"retro\"}" },
                "color": "secondary"
            }),
        ];
        json!({ "one_time": false, "buttons": [row, charts_row] }).to_string()
    }

    pub async fn call(&self, method: &str, params: &[(&str, String)]) -> Result<Value> {
        let mut form: Vec<(&str, String)> = params.to_vec();
        form.push(("access_token", self.token.clone()));
        form.push(("v", API_VERSION.into()));

        let body: Value = self
            .client
            .post(format!("{API}/{method}"))
            .form(&form)
            .send()
            .await?
            .json()
            .await
            .with_context(|| format!("vk {method}: ответ не JSON"))?;

        if let Some(err) = body.get("error") {
            bail!("vk {method}: {}", err["error_msg"].as_str().unwrap_or("unknown"));
        }
        Ok(body["response"].clone())
    }

    /// Скачивает картинку отеля, подгоняет под пропорции карусели (13:8)
    /// и загружает как фото для сообщений. Возвращает "owner_id_photo_id".
    /// Аплоад-сервер ВК изредка отдаёт мусор вместо JSON — пробуем до 3 раз.
    async fn upload_photo(&self, peer_id: i64, url: &str) -> Result<String> {
        let image = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let image = crop_to_carousel_ratio(&image);

        let mut last_err = None;
        for _ in 0..3 {
            match self.upload_photo_bytes(peer_id, image.clone()).await {
                Ok(photo) => return Ok(format!("{}_{}", photo.owner_id, photo.id)),
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                }
            }
        }
        Err(last_err.expect("после трёх неудачных попыток ошибка есть"))
    }

    async fn upload_photo_bytes(&self, peer_id: i64, image: Vec<u8>) -> Result<SavedPhoto> {
        let upload = self
            .call("photos.getMessagesUploadServer", &[("peer_id", peer_id.to_string())])
            .await?;
        let upload_url = upload["upload_url"]
            .as_str()
            .context("нет upload_url")?
            .to_string();

        let part = reqwest::multipart::Part::bytes(image)
            .file_name("photo.png")
            .mime_str("image/png")?;
        let uploaded: Value = self
            .client
            .post(upload_url)
            .multipart(reqwest::multipart::Form::new().part("photo", part))
            .send()
            .await?
            .json()
            .await?;

        let saved = self
            .call(
                "photos.saveMessagesPhoto",
                &[
                    ("server", uploaded["server"].to_string()),
                    ("photo", uploaded["photo"].as_str().unwrap_or_default().to_string()),
                    ("hash", uploaded["hash"].as_str().unwrap_or_default().to_string()),
                ],
            )
            .await?;
        let photo = &saved[0];
        Ok(SavedPhoto {
            owner_id: photo["owner_id"].as_i64().context("saveMessagesPhoto: нет owner_id")?,
            id: photo["id"].as_i64().context("saveMessagesPhoto: нет id")?,
            access_key: photo["access_key"].as_str().map(str::to_string),
        })
    }

    /// Шлёт дайджест туров: карусели по 10 карточек, по сообщению на страницу.
    /// Прогресс подготовки карточек показывается одним сообщением, которое редактируется.
    pub async fn send_digest(&self, peer_id: i64, tours: &[HotTour]) -> Result<()> {
        if tours.is_empty() {
            self.send_text(peer_id, "Сегодня горящих туров в заданном окне вылета не нашлось 🤷")
                .await?;
            return Ok(());
        }

        let total = tours.len();
        let progress_id = match self
            .send_text_id(peer_id, &format!("⏳ Готовлю карточки… 0/{total}"))
            .await
        {
            Ok(id) => Some(id),
            Err(e) => {
                eprintln!("прогресс-сообщение не отправилось: {e:#}");
                None
            }
        };
        let mut done = 0usize;

        let pages: Vec<&[HotTour]> = tours.chunks(CAROUSEL_LIMIT).collect();
        let total_pages = pages.len();

        for (page_idx, page) in pages.iter().enumerate() {
            // Фото грузим параллельно, прогресс отмечаем по мере завершения,
            // а карточки потом собираем в исходном порядке (по индексу).
            let mut photo_ids: Vec<Option<String>> = vec![None; page.len()];
            {
                let upload_futures: Vec<_> = page
                    .iter()
                    .enumerate()
                    .map(|(i, tour)| {
                        let url = tour.picture_url.clone();
                        async move { (i, self.upload_photo(peer_id, &url).await) }
                    })
                    .collect();
                let mut uploads =
                    stream::iter(upload_futures).buffer_unordered(UPLOAD_CONCURRENCY);

                while let Some((i, result)) = uploads.next().await {
                    match result {
                        Ok(id) => photo_ids[i] = Some(id),
                        Err(e) => eprintln!("фото для {} не загрузилось: {e:#}", page[i].hotel_name),
                    }
                    done += 1;
                    println!("карточка {done}/{total}: {}", page[i].hotel_name);
                    if let Some(id) = progress_id {
                        if let Err(e) = self
                            .edit_text(peer_id, id, &format!("⏳ Готовлю карточки… {done}/{total}"))
                            .await
                        {
                            eprintln!("не отредактировал прогресс: {e:#}");
                        }
                    }
                }
            }
            let elements: Vec<Value> = page
                .iter()
                .zip(photo_ids)
                .map(|(tour, photo_id)| carousel_element(tour, photo_id))
                .collect();

            let header = if total_pages > 1 {
                format!(
                    "🔥 Горящие туры {} → {}, цены за двоих (стр. {}/{})",
                    tours[0].departure,
                    tours[0].country,
                    page_idx + 1,
                    total_pages
                )
            } else {
                format!(
                    "🔥 Горящие туры {} → {}, цены за двоих",
                    tours[0].departure, tours[0].country
                )
            };

            // ВК не принимает template и keyboard в одном сообщении —
            // клавиатура остаётся от предыдущих сообщений бота.
            let template = json!({ "type": "carousel", "elements": elements }).to_string();
            let sent = self
                .call(
                    "messages.send",
                    &[
                        ("peer_id", peer_id.to_string()),
                        ("random_id", rand::random::<i32>().to_string()),
                        ("message", header.clone()),
                        ("template", template),
                    ],
                )
                .await;

            // Карусель может не пройти (например, не та платформа у получателя) —
            // тогда отправляем те же туры обычным текстом.
            if let Err(e) = sent {
                eprintln!("карусель не отправилась ({e:#}), шлю текстом");
                let text = page.iter().map(format_tour_text).collect::<Vec<_>>().join("\n\n");
                self.send_text(peer_id, &format!("{header}\n\n{text}")).await?;
            }
        }

        if let Some(id) = progress_id {
            let _ = self
                .edit_text(peer_id, id, &format!("✅ Карточки готовы: {total}"))
                .await;
        }
        Ok(())
    }

    pub async fn send_text(&self, peer_id: i64, text: &str) -> Result<()> {
        self.send_text_id(peer_id, text).await.map(|_| ())
    }

    /// Отправляет картинку (PNG-байты) с подписью.
    pub async fn send_photo(&self, peer_id: i64, png: Vec<u8>, caption: &str) -> Result<()> {
        let photo = self.upload_photo_bytes(peer_id, png).await?;
        let mut attachment = format!("photo{}_{}", photo.owner_id, photo.id);
        if let Some(key) = &photo.access_key {
            attachment = format!("{attachment}_{key}");
        }
        self.call(
            "messages.send",
            &[
                ("peer_id", peer_id.to_string()),
                ("random_id", rand::random::<i32>().to_string()),
                ("message", caption.to_string()),
                ("attachment", attachment),
                ("keyboard", self.keyboard()),
            ],
        )
        .await?;
        Ok(())
    }

    /// То же, что send_text, но возвращает id сообщения — нужен для messages.edit.
    async fn send_text_id(&self, peer_id: i64, text: &str) -> Result<i64> {
        let resp = self
            .call(
                "messages.send",
                &[
                    ("peer_id", peer_id.to_string()),
                    ("random_id", rand::random::<i32>().to_string()),
                    ("message", text.to_string()),
                    ("keyboard", self.keyboard()),
                ],
            )
            .await?;
        resp.as_i64()
            .or_else(|| resp["message_id"].as_i64())
            .context("messages.send: не вернул id сообщения")
    }

    async fn edit_text(&self, peer_id: i64, message_id: i64, text: &str) -> Result<()> {
        self.call(
            "messages.edit",
            &[
                ("peer_id", peer_id.to_string()),
                ("message_id", message_id.to_string()),
                ("message", text.to_string()),
            ],
        )
        .await?;
        Ok(())
    }
}

fn carousel_element(tour: &HotTour, photo_id: Option<String>) -> Value {
    let stars = "★".repeat(tour.hotel_stars as usize);
    let title = truncate(&format!("{} {}", tour.hotel_name, stars), 80);
    let description = truncate(
        &format!(
            "{} ₽ (было {} ₽) · {} ноч. · {}–{}",
            group_digits(tour.price),
            group_digits(tour.price_old),
            tour.nights,
            tour.fly_date.format("%d.%m"),
            tour.return_date.format("%d.%m"),
        ),
        80,
    );

    let mut element = json!({
        "title": title,
        "description": description,
        "action": { "type": "open_link", "link": tour.hotel_url },
        "buttons": [{
            "action": {
                "type": "open_link",
                "link": tour.hotel_url,
                "label": "Открыть отель"
            }
        }]
    });
    if let Some(id) = photo_id {
        element["photo_id"] = Value::String(id);
    }
    element
}

fn format_tour_text(tour: &HotTour) -> String {
    format!(
        "🏨 {} {}★ ({})\n💰 {} ₽, было {} ₽\n🌙 {} ночей, вылет {}, прилёт {}",
        tour.hotel_name,
        tour.hotel_stars,
        tour.region,
        group_digits(tour.price),
        group_digits(tour.price_old),
        tour.nights,
        tour.fly_date.format("%d.%m.%Y"),
        tour.return_date.format("%d.%m.%Y"),
    )
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars - 1).collect();
        format!("{cut}…")
    }
}

fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_to_13_8() {
        let bytes = std::fs::read("/tmp/hotel.jpg").unwrap();
        let cropped = crop_to_carousel_ratio(&bytes);
        let img = image::load_from_memory(&cropped).unwrap();
        let (w, h) = (img.width(), img.height());
        // ВК требует точное 13:8
        assert_eq!(w * 8, h * 13, "{w}x{h}");
    }
}
