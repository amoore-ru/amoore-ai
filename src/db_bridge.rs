//! Мост к Supabase REST (аналог db_bridge.py). Сейчас перенесено: нормализация SUPABASE_URL,
//! is_online (bot_status), heartbeat (upsert раз в 5 минут), register_subscriber.
//! Любая ошибка БД не должна ронять бота: is_online при сбое возвращает true.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::{Client, RequestBuilder};

use crate::tg_api::truncate_chars;

const ONLINE_TTL: Duration = Duration::from_secs(10);
pub const HB_EVERY: Duration = Duration::from_secs(5 * 60);
const HB_RETRY: Duration = Duration::from_secs(60);

/// Приводит SUPABASE_URL к корню проекта (https://<ref>.supabase.co): отрезает /rest/v1, /auth/v1 и т. п.,
/// query/fragment, кавычки и пробелы по краям; добавляет https://, если схемы нет (см. _normalize_sb_url).
pub fn normalize_sb_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches(|c: char| c == '"' || c == '\'').trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut u = trimmed.to_string();
    let low0 = u.to_ascii_lowercase();
    if !(low0.starts_with("http://") || low0.starts_with("https://")) {
        u = format!("https://{u}");
    }
    if let Some(i) = u.find(|c: char| c == '?' || c == '#') {
        u.truncate(i);
    }
    for marker in [
        "/rest/v1",
        "/auth/v1",
        "/functions/v1",
        "/storage/v1",
        "/realtime/v1",
        "/graphql/v1",
    ] {
        // ASCII-lowercase не меняет длину строки, поэтому индексы совпадают
        if let Some(k) = u.to_ascii_lowercase().find(marker) {
            if k > 0 {
                u.truncate(k);
            }
        }
    }
    u.trim_end_matches('/').to_string()
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[derive(Debug, Clone, Default)]
pub struct HbState {
    pub ok: Option<bool>,
    pub err: String,
}

pub struct DbBridge {
    http: Client,
    url: String,
    key: String,
    online_cache: Mutex<(Option<Instant>, bool)>,
    hb: Mutex<HbState>,
}

impl DbBridge {
    pub fn new(url: String, key: String) -> Self {
        Self {
            http: Client::new(),
            url,
            key,
            online_cache: Mutex::new((None, true)),
            hb: Mutex::new(HbState::default()),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.url.is_empty() && !self.key.is_empty()
    }

    pub fn heartbeat_state(&self) -> HbState {
        self.hb.lock().unwrap().clone()
    }

    fn auth(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("apikey", &self.key).bearer_auth(&self.key)
    }

    /// false ТОЛЬКО если в bot_status явно is_online = false; результат кэшируется на 10 с.
    pub async fn is_online(&self) -> bool {
        if !self.enabled() {
            return true;
        }
        {
            let c = self.online_cache.lock().unwrap();
            if let (Some(t), v) = (c.0, c.1) {
                if t.elapsed() < ONLINE_TTL {
                    return v;
                }
            }
        }
        let mut value = true;
        let res = self
            .auth(self.http.get(format!(
                "{}/rest/v1/bot_status?id=eq.main&select=is_online",
                self.url
            )))
            .header("Prefer", "return=representation")
            .timeout(Duration::from_secs(3))
            .send()
            .await;
        match res {
            Ok(r) if r.status().as_u16() == 200 => {
                if let Ok(rows) = r.json::<Vec<serde_json::Value>>().await {
                    if let Some(first) = rows.first() {
                        value = first.get("is_online").and_then(|v| v.as_bool()) != Some(false);
                    }
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("is_online: {e}"),
        }
        *self.online_cache.lock().unwrap() = (Some(Instant::now()), value);
        value
    }

    /// Пульс → bot_status.last_heartbeat. UPSERT (создаёт строку id='main' при отсутствии, is_online не трогает),
    /// две попытки, ответ проверяется и пишется в лог.
    pub async fn heartbeat(&self) -> bool {
        if !self.enabled() {
            return false;
        }
        let stamp = now_iso();
        let mut err = String::new();
        for attempt in 0..2 {
            let res = self
                .auth(
                    self.http
                        .post(format!("{}/rest/v1/bot_status?on_conflict=id", self.url)),
                )
                .header("Prefer", "resolution=merge-duplicates,return=minimal")
                .json(&serde_json::json!({ "id": "main", "last_heartbeat": stamp }))
                .timeout(Duration::from_secs(8))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    *self.hb.lock().unwrap() = HbState { ok: Some(true), err: String::new() };
                    return true;
                }
                Ok(r) => {
                    let status = r.status().as_u16();
                    let body = r.text().await.unwrap_or_default();
                    err = format!("HTTP {status}: {}", truncate_chars(&body, 160));
                    if body.contains("PGRST125") {
                        err.push_str(&format!(
                            " | SUPABASE_URL должен быть корнем проекта https://<ref>.supabase.co без /rest/v1 (сейчас: {})",
                            self.url
                        ));
                    } else if status == 404 && body.contains("PGRST205") {
                        err.push_str(" | таблицы bot_status нет: выполните 004_v55_stability.sql");
                    } else if status == 401 || status == 403 {
                        err.push_str(" | ключ не подходит: нужен SUPABASE_SERVICE_ROLE_KEY (service_role) от того же проекта, что и SUPABASE_URL");
                    }
                }
                Err(e) => err = truncate_chars(&e.to_string(), 160),
            }
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
        }
        tracing::warn!("heartbeat failed: {err}");
        *self.hb.lock().unwrap() = HbState { ok: Some(false), err };
        false
    }

    /// Фоновый пульс: раз в 5 минут, после неудачи — через минуту.
    pub fn spawn_heartbeat_loop(self: &std::sync::Arc<Self>) {
        if !self.enabled() {
            tracing::info!("supabase не настроен: heartbeat отключён");
            return;
        }
        let db = self.clone();
        tokio::spawn(async move {
            loop {
                let ok = db.heartbeat().await;
                tokio::time::sleep(if ok { HB_EVERY } else { HB_RETRY }).await;
            }
        });
        tracing::info!("supabase bridge: heartbeat запущен");
    }

    /// Upsert пользователя Telegram для сервисных объявлений (только service-role).
    pub async fn register_subscriber(
        &self,
        telegram_id: i64,
        chat_id: i64,
        username: &str,
        first_name: &str,
        last_name: &str,
    ) {
        if !self.enabled() {
            return;
        }
        let res = self
            .auth(self.http.post(format!(
                "{}/rest/v1/bot_subscribers?on_conflict=telegram_id",
                self.url
            )))
            .header("Prefer", "resolution=merge-duplicates,return=minimal")
            .json(&serde_json::json!({
                "telegram_id": telegram_id,
                "chat_id": chat_id,
                "username": truncate_chars(username, 128),
                "first_name": truncate_chars(first_name, 128),
                "last_name": truncate_chars(last_name, 128),
                "is_active": true,
                "last_seen_at": now_iso(),
            }))
            .timeout(Duration::from_secs(6))
            .send()
            .await;
        if let Err(e) = res {
            tracing::debug!("register_subscriber: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_root_is_kept() {
        assert_eq!(normalize_sb_url("https://abc.supabase.co"), "https://abc.supabase.co");
        assert_eq!(normalize_sb_url("https://abc.supabase.co/"), "https://abc.supabase.co");
    }

    #[test]
    fn url_tails_are_cut() {
        assert_eq!(normalize_sb_url("https://abc.supabase.co/rest/v1"), "https://abc.supabase.co");
        assert_eq!(normalize_sb_url("https://abc.supabase.co/rest/v1/"), "https://abc.supabase.co");
        assert_eq!(normalize_sb_url("https://abc.supabase.co/auth/v1/token?x=1"), "https://abc.supabase.co");
        assert_eq!(normalize_sb_url("https://abc.supabase.co/?apikey=1#x"), "https://abc.supabase.co");
    }

    #[test]
    fn url_scheme_quotes_and_spaces() {
        assert_eq!(normalize_sb_url("  \"abc.supabase.co\"  "), "https://abc.supabase.co");
        assert_eq!(normalize_sb_url("'HTTPS://abc.supabase.co/REST/V1'"), "HTTPS://abc.supabase.co");
        assert_eq!(normalize_sb_url(""), "");
        assert_eq!(normalize_sb_url("  "), "");
    }
}
