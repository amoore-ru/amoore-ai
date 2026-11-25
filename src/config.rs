//! Конфигурация из окружения. Имена переменных — те же, что у Python-бота (bot.py / db_bridge.py / ai_logic.py).
use anyhow::{bail, Result};
use std::env;

use crate::db_bridge::normalize_sb_url;

#[derive(Debug, Clone)]
pub struct Config {
    pub bot_token: String,
    /// Основатель (жёстко по OWNER_ID); 0 — не задан.
    pub owner_id: i64,
    pub webhook_secret: String,
    /// GROQ_API_KEY, GROQ_API_KEY_1…_9: порядок перечисления, дубли отброшены.
    pub groq_keys: Vec<String>,
    pub supabase_url: String,
    pub supabase_key: String,
    pub port: u16,
}

fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn first_of(names: &[&str]) -> String {
    names.iter().find_map(|n| var(n)).unwrap_or_default()
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let Some(bot_token) = var("TELEGRAM_BOT_TOKEN") else {
            bail!("не задан TELEGRAM_BOT_TOKEN");
        };
        let Some(webhook_secret) = var("WEBHOOK_SECRET") else {
            bail!("не задан WEBHOOK_SECRET (без него вебхук открыт для посторонних)");
        };

        let mut groq_keys: Vec<String> = Vec::new();
        let names = std::iter::once("GROQ_API_KEY".to_string())
            .chain((1..=9).map(|i| format!("GROQ_API_KEY_{i}")));
        for n in names {
            if let Some(k) = var(&n) {
                if !groq_keys.contains(&k) {
                    groq_keys.push(k);
                }
            }
        }

        Ok(Self {
            bot_token,
            owner_id: var("OWNER_ID").and_then(|v| v.parse().ok()).unwrap_or(0),
            webhook_secret,
            groq_keys,
            supabase_url: normalize_sb_url(&first_of(&["SUPABASE_URL", "AMOORE_SUPABASE_URL"])),
            // service_role предпочтительнее: серверные записи; anon — запасной вариант с ограниченными правами
            supabase_key: first_of(&[
                "SUPABASE_SERVICE_ROLE_KEY",
                "SUPABASE_SERVICE_KEY",
                "SUPABASE_ANON_KEY",
                "AMOORE_SUPABASE_ANON",
            ]),
            port: var("PORT").and_then(|v| v.parse().ok()).unwrap_or(10000),
        })
    }

    pub fn is_owner(&self, user_id: i64) -> bool {
        self.owner_id != 0 && user_id == self.owner_id
    }
}
