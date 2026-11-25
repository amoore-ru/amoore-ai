//! Логика ИИ (аналог ai_logic.py): пул ключей Groq с охлаждением, экономия токенов на запасных ключах,
//! перебор моделей, личность amoore, память диалога (в оперативной памяти), ответ «создатель выключил».
//! Перенесено: Groq-чат. Ещё НЕ перенесено: OpenRouter и Google AI как запасные,
//! vision, живой поиск (ai_boost.py), статистика для /status.
//!
//! Правила (ARCHITECTURE.md §3):
//! * порядок попыток: ключ 1 → ключ 2 → …; на каждом ключе перебираются CHAT_MODELS;
//! * 429 → ключ «остывает» Retry-After секунд (по умолчанию 60) и берётся следующий; если ключ единственный,
//!   429 считается лимитом модели и пробуется следующая модель;
//! * 401/403 → ключ отключается на час; 404/410/«decommissioned» → модель отключается на час;
//! * основной ключ получает полный контекст, запасные — compact_messages (системный промпт + последние 3);
//! * если все ключи в паузе — паузы сбрасываются и делается ещё одна попытка.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const FALLBACK_KEEP_LAST: usize = 3;
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);
pub const DEAD_KEY_COOLDOWN: Duration = Duration::from_secs(3600);
pub const MODEL_DOWN_FOR: Duration = Duration::from_secs(3600);
pub const CHAT_MODELS: [&str; 2] = ["openai/gpt-oss-120b", "openai/gpt-oss-20b"];

/// Личность. Никаких шаблонных ответов: всё, что бот пишет в чате, генерирует модель.
pub const PERSONA: &str = "Ты amoore, девушка, эмпатичный ИИ. У тебя есть память: помни, о чём вы говорили раньше в этом чате. \
Отвечай свободно, без шаблонов и канцелярита, живо и тепло, на языке собеседника (по умолчанию по-русски). \
Никогда не показывай внутренние рассуждения и теги <think>. Если пишешь код, всегда дописывай его целиком.";

/// Режим «тумблер выключен» (bot_status.is_online = false): ответ тоже генерирует модель, не шаблон.
pub const OFFLINE_PROMPT: &str = "Создатель выключил тебя. Ответь от лица amoore (девушки), что ты недоступна, но скоро вернёшься. \
Скажи это коротко, тепло и своими словами; не выдумывай причин и сроков и не отвечай на сам вопрос по существу.";

/// Сколько последних реплик (пользователь + ответ считаются по одной) помним на чат.
pub const HISTORY_MAX: usize = 20;
/// Защита от разрастания памяти процесса: при превышении число чатов в истории сбрасывается.
pub const HISTORY_CHATS_MAX: usize = 2000;
const USER_TEXT_MAX: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: &str) -> Self {
        Self { role: role.into(), content: content.into() }
    }
}

/// Системный промпт + последние `keep_last` сообщений диалога; диалог начинается с реплики пользователя.
pub fn compact_messages(msgs: &[ChatMessage], keep_last: usize) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = msgs.iter().filter(|m| m.role == "system").take(1).cloned().collect();
    let dialog: Vec<&ChatMessage> = msgs.iter().filter(|m| m.role != "system").collect();
    let start = dialog.len().saturating_sub(keep_last);
    let mut tail: Vec<&ChatMessage> = dialog[start..].to_vec();
    while tail.first().map_or(false, |m| m.role != "user") {
        tail.remove(0);
    }
    out.extend(tail.into_iter().cloned());
    out
}

/// Убирает рассуждения модели: блоки `<think>…</think>`; незакрытый `<think>` отбрасывает остаток.
pub fn strip_think(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// system + история + новая реплика пользователя.
pub fn build_messages(system: &str, history: &[ChatMessage], user_text: &str) -> Vec<ChatMessage> {
    let mut v = Vec::with_capacity(history.len() + 2);
    v.push(ChatMessage::new("system", system));
    v.extend(history.iter().cloned());
    v.push(ChatMessage::new("user", user_text));
    v
}

/// Память диалогов: последние HISTORY_MAX реплик на чат, только в оперативной памяти процесса
/// (после перезапуска на Render начинается с нуля).
#[derive(Default)]
pub struct ChatHistory {
    map: Mutex<HashMap<i64, VecDeque<ChatMessage>>>,
}

impl ChatHistory {
    pub fn snapshot(&self, chat_id: i64) -> Vec<ChatMessage> {
        self.map.lock().unwrap().get(&chat_id).map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn push_pair(&self, chat_id: i64, user: &str, assistant: &str) {
        let mut map = self.map.lock().unwrap();
        if map.len() >= HISTORY_CHATS_MAX && !map.contains_key(&chat_id) {
            map.clear();
        }
        let q = map.entry(chat_id).or_default();
        q.push_back(ChatMessage::new("user", user));
        q.push_back(ChatMessage::new("assistant", assistant));
        while q.len() > HISTORY_MAX {
            q.pop_front();
        }
        // диалог в истории всегда начинается с реплики пользователя
        while q.front().map_or(false, |m| m.role != "user") {
            q.pop_front();
        }
    }
}

#[derive(Debug)]
struct Key {
    value: String,
    cooldown_until: Option<Instant>,
}

pub struct KeyPool {
    keys: Mutex<Vec<Key>>,
}

impl KeyPool {
    pub fn new(values: Vec<String>) -> Self {
        let mut uniq: Vec<String> = Vec::new();
        for v in values {
            if !v.is_empty() && !uniq.contains(&v) {
                uniq.push(v);
            }
        }
        Self { keys: Mutex::new(uniq.into_iter().map(|value| Key { value, cooldown_until: None }).collect()) }
    }

    pub fn len(&self) -> usize {
        self.keys.lock().unwrap().len()
    }

    /// Ключи не в паузе в порядке перечисления; если в паузе все — паузы сбрасываются.
    pub fn ordered_at(&self, now: Instant) -> Vec<String> {
        let mut keys = self.keys.lock().unwrap();
        let free: Vec<String> = keys
            .iter()
            .filter(|k| k.cooldown_until.map_or(true, |t| t <= now))
            .map(|k| k.value.clone())
            .collect();
        if free.is_empty() && !keys.is_empty() {
            for k in keys.iter_mut() {
                k.cooldown_until = None;
            }
            return keys.iter().map(|k| k.value.clone()).collect();
        }
        free
    }

    pub fn ordered(&self) -> Vec<String> {
        self.ordered_at(Instant::now())
    }

    pub fn cooldown_at(&self, value: &str, now: Instant, dur: Duration) {
        if let Some(k) = self.keys.lock().unwrap().iter_mut().find(|k| k.value == value) {
            k.cooldown_until = Some(now + dur);
        }
    }

    pub fn cooldown(&self, value: &str, dur: Duration) {
        self.cooldown_at(value, Instant::now(), dur);
    }
}

enum Attempt {
    Ok(String),
    RateLimited(Duration),
    DeadKey,
    ModelGone,
    Other(String),
}

pub struct AiLogic {
    http: reqwest::Client,
    pool: KeyPool,
    models_down: Mutex<HashMap<String, Instant>>,
    history: ChatHistory,
}

impl AiLogic {
    pub fn new(groq_keys: Vec<String>) -> Self {
        Self { http: reqwest::Client::new(), pool: KeyPool::new(groq_keys), models_down: Mutex::new(HashMap::new()), history: ChatHistory::default() }
    }

    pub fn has_keys(&self) -> bool {
        self.pool.len() > 0
    }

    fn model_is_down(&self, model: &str) -> bool {
        let mut m = self.models_down.lock().unwrap();
        match m.get(model) {
            Some(t) if *t > Instant::now() => true,
            Some(_) => {
                m.remove(model);
                false
            }
            None => false,
        }
    }

    fn mark_model_down(&self, model: &str) {
        self.models_down.lock().unwrap().insert(model.to_string(), Instant::now() + MODEL_DOWN_FOR);
    }

    async fn call(&self, key: &str, model: &str, msgs: &[ChatMessage], temperature: f32, max_tokens: u32) -> Attempt {
        let resp = self
            .http
            .post(GROQ_URL)
            .bearer_auth(key)
            .json(&serde_json::json!({
                "model": model,
                "messages": msgs,
                "temperature": temperature,
                "max_tokens": max_tokens,
            }))
            .timeout(Duration::from_secs(60))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Attempt::Other(format!("{model}: {e}")),
        };
        let status = resp.status().as_u16();
        if status == 200 {
            return match resp.json::<serde_json::Value>().await {
                Ok(v) => match v["choices"][0]["message"]["content"].as_str() {
                    Some(t) => Attempt::Ok(t.to_string()),
                    None => Attempt::Other(format!("{model}: пустой ответ")),
                },
                Err(e) => Attempt::Other(format!("{model}: {e}")),
            };
        }
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = resp.text().await.unwrap_or_default();
        match status {
            429 => Attempt::RateLimited(retry_after.unwrap_or(DEFAULT_COOLDOWN)),
            401 | 403 => Attempt::DeadKey,
            404 | 410 => Attempt::ModelGone,
            _ if body.to_lowercase().contains("decommissioned") => Attempt::ModelGone,
            _ => Attempt::Other(format!("{model}: HTTP {status} {}", crate::tg_api::truncate_chars(&body, 200))),
        }
    }

    /// Живой ответ на свободный текст: личность + имя собеседника + память чата.
    pub async fn reply(&self, chat_id: i64, first_name: &str, text: &str) -> Result<String> {
        let mut system = PERSONA.to_string();
        let name = crate::tg_api::truncate_chars(first_name.trim(), 64);
        if !name.is_empty() {
            system.push_str(&format!("\nСобеседника зовут {name}."));
        }
        let user_text = crate::tg_api::truncate_chars(text, USER_TEXT_MAX);
        let msgs = build_messages(&system, &self.history.snapshot(chat_id), &user_text);
        let answer = strip_think(&self.chat(&msgs, 0.8, 2048).await?);
        if answer.is_empty() {
            bail!("модель вернула пустой ответ");
        }
        self.history.push_pair(chat_id, &user_text, &answer);
        Ok(answer)
    }

    /// Ответ, когда тумблер выключен (is_online = false): «ты недоступна, но скоро вернёшься» — генерирует модель.
    /// В память чата не пишется: пока бот «выключен», диалог не продолжается.
    pub async fn offline_reply(&self, text: &str) -> Result<String> {
        let system = format!("{PERSONA}\n{OFFLINE_PROMPT}");
        let user_text = crate::tg_api::truncate_chars(text, USER_TEXT_MAX);
        let msgs = build_messages(&system, &[], &user_text);
        let answer = strip_think(&self.chat(&msgs, 0.9, 400).await?);
        if answer.is_empty() {
            bail!("модель вернула пустой ответ");
        }
        Ok(answer)
    }

    /// Ответ модели или ошибка, если не сработал ни один ключ/модель.
    pub async fn chat(&self, msgs: &[ChatMessage], temperature: f32, max_tokens: u32) -> Result<String> {
        let keys = self.pool.ordered();
        if keys.is_empty() {
            bail!("нет ключей Groq (GROQ_API_KEY)");
        }
        let single = keys.len() == 1;
        let mut last_err = String::from("нет доступных моделей");
        for (i, key) in keys.iter().enumerate() {
            let payload = if i == 0 { msgs.to_vec() } else { compact_messages(msgs, FALLBACK_KEEP_LAST) };
            for model in CHAT_MODELS {
                if self.model_is_down(model) {
                    continue;
                }
                match self.call(key, model, &payload, temperature, max_tokens).await {
                    Attempt::Ok(t) => return Ok(t),
                    Attempt::RateLimited(d) => {
                        last_err = format!("{model}: 429");
                        if single {
                            continue; // единственный ключ: лимит модели → следующая модель
                        }
                        self.pool.cooldown(key, d);
                        break; // следующий ключ
                    }
                    Attempt::DeadKey => {
                        last_err = format!("{model}: ключ отклонён (401/403)");
                        self.pool.cooldown(key, DEAD_KEY_COOLDOWN);
                        break;
                    }
                    Attempt::ModelGone => {
                        last_err = format!("{model}: модель недоступна");
                        self.mark_model_down(model);
                    }
                    Attempt::Other(e) => last_err = e,
                }
            }
        }
        bail!("не удалось получить ответ от моделей: {last_err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(role: &str, c: &str) -> ChatMessage {
        ChatMessage::new(role, c)
    }

    #[test]
    fn compact_keeps_system_and_last_three_starting_with_user() {
        let msgs = vec![
            m("system", "S"),
            m("user", "1"),
            m("assistant", "2"),
            m("user", "3"),
            m("assistant", "4"),
            m("user", "5"),
        ];
        let c = compact_messages(&msgs, 3);
        assert_eq!(c, vec![m("system", "S"), m("user", "3"), m("assistant", "4"), m("user", "5")]);
    }

    #[test]
    fn compact_drops_leading_assistant_in_tail() {
        let msgs = vec![m("system", "S"), m("user", "1"), m("assistant", "2"), m("user", "3"), m("assistant", "4")];
        // хвост из 3: assistant(2), user(3), assistant(4) → начинается с user(3)
        let c = compact_messages(&msgs, 3);
        assert_eq!(c, vec![m("system", "S"), m("user", "3"), m("assistant", "4")]);
    }

    #[test]
    fn compact_short_dialog_unchanged() {
        let msgs = vec![m("system", "S"), m("user", "hi")];
        assert_eq!(compact_messages(&msgs, 3), msgs);
    }

    #[test]
    fn pool_dedupes_and_keeps_order() {
        let p = KeyPool::new(vec!["a".into(), "b".into(), "a".into(), "".into()]);
        assert_eq!(p.len(), 2);
        assert_eq!(p.ordered(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn pool_cooldown_skips_key_then_expires() {
        let p = KeyPool::new(vec!["a".into(), "b".into()]);
        let t0 = Instant::now();
        p.cooldown_at("a", t0, Duration::from_secs(60));
        assert_eq!(p.ordered_at(t0), vec!["b".to_string()]);
        assert_eq!(p.ordered_at(t0 + Duration::from_secs(61)), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn pool_resets_when_all_cooling() {
        let p = KeyPool::new(vec!["a".into(), "b".into()]);
        let t0 = Instant::now();
        p.cooldown_at("a", t0, Duration::from_secs(60));
        p.cooldown_at("b", t0, Duration::from_secs(60));
        assert_eq!(p.ordered_at(t0), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn strip_think_removes_blocks() {
        assert_eq!(strip_think("<think>рассуждаю</think>Привет!"), "Привет!");
        assert_eq!(strip_think("А <think>x</think>б<think>y</think> в"), "А б в");
        assert_eq!(strip_think("Ответ<think>обрыв без закрытия"), "Ответ");
        assert_eq!(strip_think("  без тегов  "), "без тегов");
    }

    #[test]
    fn build_messages_orders_system_history_user() {
        let h = vec![m("user", "1"), m("assistant", "2")];
        assert_eq!(build_messages("S", &h, "3"), vec![m("system", "S"), m("user", "1"), m("assistant", "2"), m("user", "3")]);
    }

    #[test]
    fn history_is_capped_and_starts_with_user() {
        let hist = ChatHistory::default();
        for i in 0..30 {
            hist.push_pair(1, &format!("u{i}"), &format!("a{i}"));
        }
        let snap = hist.snapshot(1);
        assert_eq!(snap.len(), HISTORY_MAX);
        assert_eq!(snap[0].role, "user");
        assert_eq!(snap.last().unwrap().content, "a29");
        assert!(hist.snapshot(2).is_empty());
    }

    #[test]
    fn prompts_follow_the_brief() {
        assert!(PERSONA.contains("amoore") && PERSONA.contains("есть память"));
        assert!(OFFLINE_PROMPT.starts_with("Создатель выключил тебя."));
    }
}
