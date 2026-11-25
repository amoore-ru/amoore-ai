//! Транспорт Telegram (аналог tg_api.py). Тонкая обёртка над teloxide::Bot:
//! тип чата по id, отправка с лимитом 4000 символов, ответ на callback, пинг.
use std::time::Instant;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ParseMode, ReplyMarkup};

/// Группы и супергруппы имеют отрицательные id (как chat_type_of в bot.py).
pub fn is_group_id(chat_id: i64) -> bool {
    chat_id < 0
}

/// Telegram режет сообщение на 4096; Python-бот обрезал до 4000 символов (не байт).
pub fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Экранирование для parse_mode=HTML (аналог html.escape: & < >).
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[derive(Clone)]
pub struct Tg {
    pub bot: Bot,
}

impl Tg {
    pub fn new(token: &str) -> Self {
        Self { bot: Bot::new(token) }
    }

    /// Аналог send(chat_id, text, reply_markup, parse_mode) без режима «редактировать вместо отправки»
    /// (он появится вместе с обработчиком callback).
    pub async fn send(
        &self,
        chat_id: i64,
        text: &str,
        html: bool,
        markup: Option<ReplyMarkup>,
    ) -> Result<(), teloxide::RequestError> {
        if text.is_empty() {
            return Ok(());
        }
        let mut req = self.bot.send_message(ChatId(chat_id), truncate_chars(text, 4000));
        if html {
            req = req.parse_mode(ParseMode::Html);
        }
        if let Some(m) = markup {
            req = req.reply_markup(m);
        }
        req.await.map(|_| ())
    }

    /// «печатает…»: ошибки не важны (чат мог закрыться), поэтому игнорируем.
    pub async fn typing(&self, chat_id: i64) {
        let _ = self.bot.send_chat_action(ChatId(chat_id), ChatAction::Typing).await;
    }

    /// В teloxide 0.13 (teloxide-core 0.10) id callback-запроса — обычная `String`;
    /// отдельного типа `CallbackQueryId` ещё нет, поэтому принимаем `impl Into<String>`.
    pub async fn answer_callback(
        &self,
        id: impl Into<String>,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<(), teloxide::RequestError> {
        let mut req = self.bot.answer_callback_query(id.into());
        if let Some(t) = text {
            req = req.text(t);
        }
        if show_alert {
            req = req.show_alert(true);
        }
        req.await.map(|_| ())
    }

    /// Аналог telegram_ping_ms(): время getMe в мс или None при ошибке.
    pub async fn ping_ms(&self) -> Option<f64> {
        let t0 = Instant::now();
        self.bot.get_me().await.ok()?;
        Some(t0.elapsed().as_secs_f64() * 1000.0)
    }

    pub async fn username(&self) -> Option<String> {
        self.bot.get_me().await.ok()?.username.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ids_are_negative() {
        assert!(is_group_id(-100123));
        assert!(!is_group_id(42));
    }

    #[test]
    fn truncates_by_chars_not_bytes() {
        let s = "я".repeat(5000);
        assert_eq!(truncate_chars(&s, 4000).chars().count(), 4000);
        assert_eq!(truncate_chars("abc", 4000), "abc");
    }

    #[test]
    fn escapes_html() {
        assert_eq!(html_escape("<b>&</b>"), "&lt;b&gt;&amp;&lt;/b&gt;");
    }
}
