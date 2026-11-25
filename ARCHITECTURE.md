# amoore-ai-rust — бот на Rust (каркас, v6.1.0 → цель 7.0)

Статус: **каркас написан, но ни разу не компилировался** (в среде разработки нет `cargo` и сети). Боевым остаётся Python-бот `amoore-ai/`.
Переключать вебхук на Rust-бота можно только после паритета по тестам и с работающим откатом (NEXT_STEPS.md §3).

**Стоп-правило:** новый Rust-код не пишем, пока эти шесть файлов не скомпилировались хотя бы один раз (см. `README.md`).
**Redis:** Rust-бот ключ `amoore:data` сейчас не читает и не пишет (в `config.rs` нет переменных `UPSTASH_*`). Условия, при которых это изменится, — в разделе «Формат `amoore:data`».

## Модули и соответствие Python

| Rust | Python | Перенесено | Ещё нет |
|---|---|---|---|
| `main.rs` | Flask-маршруты в `bot.py` | `POST /webhook` (секрет `X-Telegram-Bot-Api-Secret-Token`, сравнение за постоянное время, обработка в фоне, ответ 200 сразу), `GET /health`, `GET /`, graceful shutdown | `GET /tick`, фоновые циклы (заявки, дайджест) |
| `config.rs` | env в `bot.py`, `db_bridge.py`, `ai_logic.py` | `TELEGRAM_BOT_TOKEN`, `OWNER_ID`, `WEBHOOK_SECRET` (обязателен, дефолта нет), `GROQ_API_KEY[_1.._9]`, `SUPABASE_URL`, ключи Supabase (service_role → anon), `PORT` | `OPENROUTER_API_KEY*`, `GOOGLE_AI_KEY`, `RENDER_*`, Redis |
| `tg_api.rs` | `tg_api.py` + `send()` | обёртка над `teloxide::Bot`: отправка (обрезка до 4000 символов, HTML), `answerCallbackQuery`, пинг, тип чата по id | редактирование вместо отправки (`_edit_ctx`), `note_own` |
| `tg_handlers.rs` | `tg_handlers.py` + роутер `bot.py` | режим профилактики, регистрация подписчика (только личка), `rate_limited` (20 за 60 с), разбор команд и алиас `/старт`, **`/start`** (текст, `MINI_KB`, базовая панель группы) | роли и права, `/help` и ~76 остальных команд, ИИ-ответ в чате, защита групп, callback-действия панели |
| `db_bridge.rs` | `db_bridge.py` | нормализация `SUPABASE_URL`, `is_online` (кэш 10 с, при сбое `true`), `heartbeat` (upsert, раз в 5 мин, после ошибки через 1 мин), `register_subscriber` | новости, заявки, дайджест, роли, коды привязки `/link`, логи |
| `ai_logic.rs` | `ai_logic.py` | пул ключей Groq (охлаждение по `Retry-After`, 401/403 → час), перебор `CHAT_MODELS`, `compact_messages` (система + 3 последних), отключение снятых моделей | OpenRouter и Google AI как запасные, vision, живой поиск (`ai_boost.py`), статистика, `identity` |

## Контракт с Supabase (используется уже сейчас)

* `bot_status` (`id='main'`, `is_online`, `last_heartbeat`): чтение `is_online`, upsert `last_heartbeat` через `?on_conflict=id`.
* `bot_subscribers` (`telegram_id` PK): upsert через `?on_conflict=telegram_id`, поля `chat_id`, `username`, `first_name`, `last_name`, `is_active`, `last_seen_at`.
* Схема — `amoore-dev/shared/sql/001_init_database.sql` и миграции 002–008; таблицы бот трогает только service_role ключом.

## Что проверить при первой сборке (`cargo build`)

Код писался без компилятора. Места, где API teloxide 0.13 могло отличаться от моей памяти:

1. `KeyboardMarkup::resize_keyboard()` — в `tg_handlers::mini_kb`. Если метод принимает аргумент, передать `true`.
2. Поле `Message::from` (в старых версиях — метод `from()`) — в `tg_handlers`.
3. `Bot::answer_callback_query(id)` с типом `CallbackQueryId` — в `tg_api::answer_callback`.
4. Фичи: `teloxide` с `default-features = false, features = ["rustls"]`; `axum 0.7` с `http1`, `tokio`, `json`.
5. После первой успешной сборки: `cargo fmt`, `cargo clippy`, `cargo test` (юнит-тесты уже есть во всех модулях: разбор URL, пул ключей, `compact_messages`, лимитер, текст `/start`).

## Порядок переноса дальше

1. Сборка и тесты каркаса → тестовый токен, `/start` в личке и в группе, сверка с Python.
2. `/status`, `/link` (коды привязки, `secrets`-аналог — `getrandom`/`rand`), `/digest` и рассылки — они покрыты Python-тестами и паритетом JS↔Python.
3. Роли и права → `/help`, панель, callback-действия.
4. ИИ-ответ: `identity` (`include_str!` на `amoore_identity.txt`), память и история, OpenRouter/Google, vision.
5. Хранилище памяти: формат описан ниже; доступ Rust к `amoore:data` — только по условиям из последнего подраздела.

## Формат `amoore:data` (Redis / Upstash) — как его читает и пишет Python

Источник: `amoore-ai/storage.py` (транспорт, загрузка, сохранение) и `amoore-ai/bot.py` (содержимое). Состояние хранится **одним JSON-документом в одном ключе**.

### Транспорт (Upstash REST)

* Чтение: `GET {UPSTASH_REDIS_REST_URL}/get/amoore:data`, заголовок `Authorization: Bearer {UPSTASH_REDIS_REST_TOKEN}`. Ответ — `{"result": "<строка>"}`; `result` — это **строка с JSON** (не вложенный объект), либо `null`, если ключа нет.
* Запись: `POST {URL}/set/amoore:data`, тело запроса — сырой JSON-текст в UTF-8 (`json.dumps(..., ensure_ascii=False)`, кириллица не экранируется), опционально `EX` как query-параметр (для `amoore:data` не используется: TTL нет).
* Каждая запись **заменяет документ целиком**. Версий, блокировок и сравнения-и-замены нет: **побеждает последний писавший**.
* Рядом лежит служебный ключ `amoore:ping` (запись с `EX=30`) — только для замера пинга Redis в `/status`; на данные не влияет.
* Запасной вариант: если Redis не настроен или не ответил, Python читает/пишет файл `data.json` рядом с `bot.py` с тем же содержимым (на Render free он эфемерный). Порядок загрузки: Redis → `data.json` → пустые значения по умолчанию.

### Загрузка и сохранение

* Загрузка (`load` → `_merge`) — **поверхностное** слияние: `_mem.update(stored)`. Каждый ключ верхнего уровня из хранилища **целиком заменяет** значение по умолчанию; вложенные значения по умолчанию не подмешиваются. Ключи, которых в хранилище нет, остаются по умолчанию.
* `stats.users` в памяти — множество (`set`), в JSON — **массив строк**; при загрузке список превращается в множество.
* Сохранение (`save`): снимок `_mem` → `stats.users` в список → `json.dumps`. Вызывается после мутаций; лёгкая статистика пишется не чаще раза в 20 секунд (`SOFT_SAVE_INTERVAL`), плюс `atexit`.

### Схема документа

Типы свободные: Python не валидирует схему, поля добавляются по мере появления функций, `null` и отсутствие поля часто равнозначны (`.get(...)`).

| Ключ верхнего уровня | Тип | Содержимое |
|---|---|---|
| `users` | объект: `"<user_id>"` → пользователь | профили; **ключ — id человека строкой, не id чата** |
| `chats` | объект: `"<chat_id>"` → данные чата | ключ — id чата строкой; у групп он отрицательный |
| `settings` | объект | `system` (пусто → берётся `[base]` из `amoore_identity.txt`), `system_group`, `emoji` (по умолчанию `"🤖"`), `enabled`, `role_labels` (объект роль → подпись), `role_permissions` |
| `phrases` | массив строк | общие фразы для всех чатов |
| `stats` | объект | `msgs` (число), `users` (массив строк — id), `today_msgs`, `today_key` (`"YYYY-MM-DD"`) |
| `reminders` | массив | `{"id": <мс, int>, "chat_id", "user_id", "due": <epoch, сек>, "text", "done": bool}` |
| `audit_log` | массив | последние 2000 записей: `{"ts": <epoch>, "actor": str, "actor_id"?: int, "role": str, "text": str, "chat_id"?: int}` |
| `action_stats` | объект | `"<actor_id>"` → `{"<команда>": счётчик}` |
| `business` | объект | `"<connection_id>"` → настройки бизнес-подключения (есть `enabled`) |
| `bday_done_days` | массив строк | ключи вида `"bday_done:YYYY-MM-DD…"`, хранятся ~3 дня |

Необязательные ключи верхнего уровня (`reminders`, `audit_log`, `action_stats`, `business`, `bday_done_days`) создаются при первом использовании через `setdefault`, их может не быть в старых документах.

**`users["<id>"]`** — значения по умолчанию: `role: "user"`, `facts: []`, `history: []`, `name: ""`, `username: ""`, `banned: false`, `allow_swear: false`. Дальше код добавляет: `memory` (текст до 600 символов), `mem_n` (счётчик), `muted_until` (epoch или `null`), `bday` и связанные `bday_congrats`, `bday_remind_days`, `perm_overrides` (личные ограничения прав), `pending` (незавершённый диалог кнопки), `stats`. Элементы `history` — `{"role": "user"|"assistant", "content": str}`.

**`chats["<id>"]`** — значения по умолчанию: `phrases: []`, `emoji: null`, `local_admins: []` (массив **чисел** — id пользователей). Дальше добавляются: `welcome`, `title`, `first_seen`, `msgs`, `activity`, `warns`, `notes`, `triggers`, `captcha`, `protect`, `banned_users` (массив чисел), `muted_users` (объект `"<user_id>"` → epoch), `mod_ranks`, `rank_perms`, `saved_permissions`, `creator_override`, `dev_mode`, `xfer`, `personal`, `last_lively`.

Все моменты времени — **epoch в секундах (число с плавающей точкой)**, кроме `reminders[].id` (миллисекунды, целое).

Список полей собран разбором кода `bot.py` (обращения `u[...]`, `cdata[...]`, `.get(...)`), а не из реального снимка данных. Перед тем как Rust начнёт работать с этим ключом, нужно сверить его с живым документом (см. ниже).

### Правила для Rust

1. **Не терять неизвестные поля.** Каждая структура (`State`, `User`, `Chat`, `Settings`, `Stats`) получает `#[serde(flatten)] extra: serde_json::Map<String, Value>`, чтобы всё, чего Rust не знает, возвращалось в документ без изменений. Пока поле не перенесено, оно хранится как есть.
2. **Сохранять формы значений:** ключи `users`/`chats` — строки; `stats.users` — массив строк; `local_admins` и `banned_users` — числа; время — число (`f64`), `reminders[].id` — целое; `null` не превращать в отсутствие и наоборот, где это различимо.
3. **Инвариант для Python:** после записи документ обязан содержать объекты `users`, `chats`, `settings` и `stats`, а в `stats` — `msgs` и массив `users`. Иначе Python упадёт на `_mem["stats"]["users"].add(...)` (при загрузке подмешиваются только присутствующие ключи верхнего уровня, и `stats` заменяется целиком).
4. **Кодировка:** UTF-8, кириллица без `\uXXXX`-экранирования (для Python неважно, но так документ остаётся читаемым и сравнимым).
5. **Размер:** весь документ — одна строка в одном запросе. `/status` Python-бота показывает `payload_kb`; прежде чем Rust будет писать, проверить лимит размера запроса в консоли Upstash.

### Условия, при которых Rust получает доступ к `amoore:data`

Два бота, пишущие в один ключ «заменой всего документа», **неизбежно затирают данные друг друга** (побеждает последний). Поэтому:

1. **Сначала только чтение**, на тестовом токене и отдельной копии данных. Доступ на запись к `amoore:data` Rust получает только когда он единственный писатель (после переключения вебхука), а Python выключен или переведён в режим «только чтение».
2. Пока идёт параллельная работа, тестовый Rust-бот использует **собственный ключ** (например, `amoore:rust:data`) либо вообще не использует Redis.
3. Перед включением записи должны пройти тесты на реальном (обезличенном) снимке документа: (а) `десериализация → сериализация` в Rust даёт тот же JSON с точностью до порядка ключей; (б) Python успешно загружает документ, записанный Rust; (в) повторный круг Python → Rust → Python ничего не теряет.
4. Для (3а–в) нужен реальный снимок: выгрузить значение ключа `amoore:data` (через Upstash-консоль или `GET …/get/amoore:data`), убрать личные данные (`history`, `facts`, `memory`, имена) и положить в `amoore-ai-rust/tests/fixtures/`. **Токены и ключи в репозиторий не класть.**
