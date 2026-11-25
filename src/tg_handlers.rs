//! Роутер апдейтов и хендлеры (аналог handle_message / handle_callback / cmd_* из bot.py и tg_handlers.py).
//! Перенесено: ИИ-ответ на свободный текст, «умное отключение» (is_online = false → ответ генерирует модель),
//! регистрация подписчика, ограничитель частоты, разбор команд, /start.
//! Шаблонных ответов нет: ни «неизвестная команда», ни «профилактика», ни «слишком много сообщений».
//! Не перенесено: роли и права, панель, /help и остальные команды, защита групп.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use teloxide::types::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, Message,
    ReplyMarkup, Update, UpdateKind,
};

use crate::ai_logic::AiLogic;
use crate::config::Config;
use crate::db_bridge::DbBridge;
use crate::tg_api::{html_escape, is_group_id, truncate_chars, Tg};


/// Общее состояние бота; живёт в Arc и раздаётся задачам.
pub struct AppState {
    pub cfg: Config,
    pub tg: Tg,
    pub db: Arc<DbBridge>,
    pub ai: AiLogic,
    pub limiter: RateLimiter,
    /// username бота без «@» (для распознавания обращений в группах); пусто, если getMe не ответил.
    pub bot_username: String,
}

impl AppState {
    pub fn new(cfg: Config, tg: Tg, db: Arc<DbBridge>, ai: AiLogic, bot_username: String) -> Self {
        Self { cfg, tg, db, ai, limiter: RateLimiter::default(), bot_username }
    }
}

// ---------- ограничитель частоты (rate_limited из tg_handlers.py) ----------
#[derive(Default)]
pub struct RateLimiter {
    map: Mutex<HashMap<i64, VecDeque<Instant>>>,
}

impl RateLimiter {
    /// true, если из этого чата за `window` уже пришло `limit` сообщений.
    pub fn limited_at(&self, chat_id: i64, now: Instant, limit: usize, window: Duration) -> bool {
        let mut map = self.map.lock().unwrap();
        let q = map.entry(chat_id).or_default();
        while let Some(front) = q.front() {
            if now.saturating_duration_since(*front) > window {
                q.pop_front();
            } else {
                break;
            }
        }
        if q.len() >= limit {
            return true;
        }
        q.push_back(now);
        false
    }

    pub fn limited(&self, chat_id: i64) -> bool {
        self.limited_at(chat_id, Instant::now(), 20, Duration::from_secs(60))
    }
}

// ---------- разбор команд ----------
/// "/Start@amoore_bot a b" → ("/start", ["a", "b"]); русские алиасы приводятся к основной команде.
pub fn parse_command(text: &str) -> Option<(String, Vec<&str>)> {
    if !text.starts_with('/') {
        return None;
    }
    let mut parts = text.split_whitespace();
    let first = parts.next()?;
    let cmd = first.split('@').next().unwrap_or(first).to_lowercase();
    let cmd = match cmd.as_str() {
        "/старт" => "/start".to_string(), // COMMAND_ALIASES; остальные алиасы приедут вместе со своими командами
        _ => cmd,
    };
    Some((cmd, parts.collect()))
}

// ---------- /start ----------
pub fn start_text(first_name: &str) -> String {
    let name = if first_name.trim().is_empty() { "друг" } else { first_name };
    format!(
        "Привет, {}! 👋 Я amoore — твоя ИИ-подруга.\nЖми кнопки ниже или просто пиши мне.\n(Полная панель управления — команда /panel)",
        html_escape(name)
    )
}

/// Reply-клавиатура личного чата (MINI_KB).
pub fn mini_kb() -> KeyboardMarkup {
    // TODO: у Python-версии клавиатура сжимается по высоте (resize_keyboard). Сигнатура этого метода
    // в teloxide-core 0.10.1 не сверена, поэтому пока не вызываем: это только внешний вид, на логику не влияет.
    KeyboardMarkup::new(vec![
        vec![KeyboardButton::new("🛠 Мои права"), KeyboardButton::new("🎛 Панель")],
        vec![KeyboardButton::new("🛠 Тех. поддержка")],
    ])
}

/// Базовые кнопки панели для группы. Кнопки, зависящие от ролей и прав (статус, фразы, эмодзи,
/// модерация, защита…), добавятся после переноса модуля прав.
pub fn group_panel_kb() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("ℹ️ Помощь", "help"),
            InlineKeyboardButton::callback("👤 Профиль", "me_profile"),
        ],
        vec![
            InlineKeyboardButton::callback("⏰ Напоминания", "me_reminders"),
            InlineKeyboardButton::callback("🎲 Кубик", "roll_кубик"),
        ],
    ])
}

async fn cmd_start(st: &AppState, chat_id: i64, first_name: &str) {
    let markup: ReplyMarkup = if is_group_id(chat_id) {
        group_panel_kb().into()
    } else {
        mini_kb().into()
    };
    if let Err(e) = st.tg.send(chat_id, &start_text(first_name), true, Some(markup)).await {
        tracing::warn!("/start: sendMessage: {e}");
    }
}

// ---------- «умное отключение» (тумблер bot_status.is_online) ----------
/// true — апдейт заблокирован (ответ уже отправлен), дальше не обрабатываем.
/// Шаблонов нет: когда тумблер выключен, ответ «я недоступна, но скоро вернусь» пишет модель.
async fn offline_gate(st: &AppState, update: &Update) -> bool {
    if st.db.is_online().await {
        return false;
    }
    // Основатель проходит всегда. TODO: пропускать и админов, когда будет перенесён модуль ролей.
    let sender = match &update.kind {
        UpdateKind::Message(m) | UpdateKind::EditedMessage(m) => m.from().map(|u| u.id.0 as i64),
        UpdateKind::CallbackQuery(cq) => Some(cq.from.id.0 as i64),
        _ => None,
    };
    if sender.map_or(false, |id| st.cfg.is_owner(id)) {
        return false;
    }
    match &update.kind {
        UpdateKind::CallbackQuery(cq) => {
            // всплывающее окно у кнопки: короткая реплика от модели (лимит Telegram — 200 символов)
            let text = st.ai.offline_reply("(человек нажал кнопку в меню)").await.ok().map(|t| truncate_chars(&t, 190));
            if let Err(e) = st.tg.answer_callback(cq.id.clone(), text.as_deref(), text.is_some()).await {
                tracing::warn!("offline callback reply failed: {e}");
            }
            true
        }
        UpdateKind::Message(m) | UpdateKind::EditedMessage(m) => {
            let chat_id = m.chat.id.0;
            let text = m.text().unwrap_or("").trim();
            // в группах реагируем только на команды, в личке — на любой текст; за флуд молчим
            if text.is_empty() || (!m.chat.is_private() && !text.starts_with('/')) || st.limiter.limited(chat_id) {
                return true;
            }
            st.tg.typing(chat_id).await;
            match st.ai.offline_reply(text).await {
                Ok(answer) => {
                    if let Err(e) = st.tg.send(chat_id, &answer, false, None).await {
                        tracing::warn!("offline reply failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("offline_reply: {e}"),
            }
            true
        }
        _ => false,
    }
}

// ---------- точка входа для апдейта ----------
pub async fn handle_update(st: Arc<AppState>, update: Update) {
    if offline_gate(&st, &update).await {
        return;
    }
    match update.kind {
        UpdateKind::Message(msg) => handle_message(&st, msg).await,
        UpdateKind::CallbackQuery(cq) => handle_callback(&st, cq).await,
        _ => {}
    }
}

async fn handle_message(st: &Arc<AppState>, msg: Message) {
    let Some(user) = msg.from() else { return };
    let chat_id = msg.chat.id.0;
    let user_id = user.id.0 as i64;
    let first_name = user.first_name.clone();

    // подписчики для сервисных объявлений — только личные чаты (как в webhook() Python-версии)
    if msg.chat.is_private() {
        let db = st.db.clone();
        let username = user.username.clone().unwrap_or_default();
        let last_name = user.last_name.clone().unwrap_or_default();
        let first = first_name.clone();
        tokio::spawn(async move {
            db.register_subscriber(user_id, chat_id, &username, &first, &last_name).await;
        });
    }

    let Some(text) = msg.text().map(str::trim).filter(|t| !t.is_empty()) else { return };

    if st.limiter.limited(chat_id) {
        return; // молча: лимит — защита от флуда, шаблонных ответов нет
    }

    if let Some((cmd, _args)) = parse_command(text) {
        if cmd == "/start" {
            cmd_start(st, chat_id, &first_name).await;
            return;
        }
        // остальные команды пока не перенесены: «неизвестной команды» нет — текст уходит в ИИ как обычная реплика
    }

    // В личке отвечаем на всё; в группе — только на обращение (команда, @упоминание, ответ на сообщение бота).
    if !msg.chat.is_private() && !is_addressed_in_group(st, &msg, text) {
        return;
    }

    // Свободный текст → ИИ. Если все ключи/модели недоступны — пишем в лог и молчим (шаблонов нет).
    st.tg.typing(chat_id).await;
    match st.ai.reply(chat_id, &first_name, text).await {
        Ok(answer) => {
            if let Err(e) = st.tg.send(chat_id, &answer, false, None).await {
                tracing::warn!("sendMessage: {e}");
            }
        }
        Err(e) => tracing::warn!("ai.reply: {e}"),
    }
}

/// Обращение к боту в группе: команда, @username в тексте или ответ на сообщение самого бота.
fn is_addressed_in_group(st: &AppState, msg: &Message, text: &str) -> bool {
    if text.starts_with('/') {
        return true;
    }
    let me = st.bot_username.to_lowercase();
    if me.is_empty() {
        return false;
    }
    if text.to_lowercase().contains(&format!("@{me}")) {
        return true;
    }
    msg.reply_to_message()
        .and_then(|r| r.from())
        .map_or(false, |u| u.is_bot && u.username.as_deref().map_or(false, |n| n.to_lowercase() == me))
}

async fn handle_callback(st: &Arc<AppState>, cq: CallbackQuery) {
    // Снимаем «часики» с кнопки; сами действия панели переедут вместе с модулем прав.
    if let Err(e) = st.tg.answer_callback(cq.id.clone(), None, false).await {
        tracing::debug!("answerCallbackQuery: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_command_with_bot_suffix_and_args() {
        let (cmd, args) = parse_command("/Start@amoore_bot  a   b").unwrap();
        assert_eq!(cmd, "/start");
        assert_eq!(args, vec!["a", "b"]);
    }

    #[test]
    fn russian_alias_maps_to_start() {
        assert_eq!(parse_command("/старт").unwrap().0, "/start");
    }

    #[test]
    fn plain_text_is_not_a_command() {
        assert!(parse_command("привет /start").is_none());
        assert!(parse_command("").is_none());
    }

    #[test]
    fn start_text_escapes_html_and_defaults_name() {
        assert!(start_text("<b>Аня</b>").starts_with("Привет, &lt;b&gt;Аня&lt;/b&gt;! 👋 Я amoore"));
        assert!(start_text("  ").starts_with("Привет, друг! 👋"));
        assert!(start_text("Аня").contains("команда /panel"));
    }

    #[test]
    fn limiter_blocks_after_limit_and_recovers() {
        let rl = RateLimiter::default();
        let t0 = Instant::now();
        let w = Duration::from_secs(60);
        for _ in 0..20 {
            assert!(!rl.limited_at(7, t0, 20, w));
        }
        assert!(rl.limited_at(7, t0, 20, w));
        assert!(!rl.limited_at(8, t0, 20, w)); // другой чат не затронут
        assert!(!rl.limited_at(7, t0 + Duration::from_secs(61), 20, w));
    }
}
