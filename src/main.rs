//! amoore-ai-rust — точка входа. Принимает вебхук Telegram так же, как Flask-версия:
//! POST /webhook (проверка X-Telegram-Bot-Api-Secret-Token), GET /health, GET /.
mod ai_logic;
mod config;
mod db_bridge;
mod tg_api;
mod tg_handlers;

use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use teloxide::types::Update;

use crate::ai_logic::AiLogic;
use crate::config::Config;
use crate::db_bridge::DbBridge;
use crate::tg_api::Tg;
use crate::tg_handlers::{handle_update, AppState};

/// Сравнение секрета за постоянное время (длина секрета не секретна).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn webhook(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> (StatusCode, &'static str) {
    let got = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !ct_eq(got.as_bytes(), st.cfg.webhook_secret.as_bytes()) {
        return (StatusCode::FORBIDDEN, "forbidden");
    }
    match serde_json::from_slice::<Update>(&body) {
        // Telegram ждёт быстрый ответ: обработку уводим в фон и сразу отвечаем 200
        Ok(update) => {
            tokio::spawn(handle_update(st.clone(), update));
        }
        Err(e) => tracing::warn!("не удалось разобрать апдейт: {e}"),
    }
    (StatusCode::OK, "ok")
}

async fn health(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "supabase": st.db.enabled(), "ai_keys": st.ai.has_keys() }))
}

async fn root() -> &'static str {
    "amoore bot (rust) is running 🤖"
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("получен сигнал остановки");
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::from_env()?;
    let tg = Tg::new(&cfg.bot_token);
    let db = Arc::new(DbBridge::new(cfg.supabase_url.clone(), cfg.supabase_key.clone()));
    let ai = AiLogic::new(cfg.groq_keys.clone());
    let port = cfg.port;

    let bot_username = match tg.username().await {
        Some(u) => {
            tracing::info!("бот: @{u}");
            u
        }
        None => {
            tracing::warn!("getMe не ответил — проверьте TELEGRAM_BOT_TOKEN и сеть");
            String::new()
        }
    };
    db.spawn_heartbeat_loop();

    let state = Arc::new(AppState::new(cfg, tg, db, ai, bot_username));
    let app = Router::new()
        .route("/webhook", post(webhook))
        .route("/health", get(health))
        .route("/", get(root))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("слушаю порт {port}");
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}
