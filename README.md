# amoore-ai-rust — как собрать каркас локально (Mac / Windows)

Цель этого этапа одна: **чтобы шесть файлов в `src/` скомпилировались и прошли 17 юнит-тестов**. Бота никуда не деплоим, вебхук боевого бота не трогаем: боевой остаётся Python (`amoore-ai/`).
Код писался без компилятора, поэтому ошибки при первой сборке ожидаемы — их нужно просто прислать (см. «Если сборка упала»).

## 1. Что установить

**Rust (одинаково для Mac и Windows)** — через `rustup`, версия не ниже **1.80** (так указано в `Cargo.toml`):

* Mac / Linux: в Терминале `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh` → выбрать пункт 1 (стандартная установка) → открыть новое окно Терминала.
* Windows: скачать `rustup-init.exe` с https://rustup.rs и запустить.

**Компилятор C (нужен зависимостям TLS; OpenSSL не нужен — используется rustls):**

* Mac: `xcode-select --install` (Command Line Tools).
* Windows: Visual Studio Build Tools с компонентом **«Разработка классических приложений на C++»** (MSVC и Windows SDK). `rustup-init` сам подскажет, если их нет.

Проверка (в новом окне терминала / PowerShell):

```
rustc --version
cargo --version
```

Если `rustc` старше 1.80 — `rustup update stable`.

## 2. Сборка и тесты

Из папки проекта (в распакованном архиве):

```
cd amoore-ai-rust
cargo build
cargo test
```

Что должно получиться:

* `cargo build` — при первом запуске скачивает и собирает зависимости (несколько минут), в конце строка `Finished dev profile …`. Предупреждения (`warning:`) допустимы, ошибки (`error[E…]`) — нет. Создаётся `Cargo.lock` — его нужно **сохранить** и положить обратно в репозиторий.
* `cargo test` — в конце `test result: ok. 17 passed; 0 failed`. Тесты есть в `tg_api` (3), `db_bridge` (3), `ai_logic` (6), `tg_handlers` (5); сеть и токены им не нужны.

Если обе команды зелёные, дополнительно:

```
cargo fmt
cargo clippy
```

`cargo fmt` только переформатирует код; `clippy` выдаст замечания — присылать их тоже полезно, но блокирующими они не являются.

## 3. Быстрая проверка запуска (по желанию, нужен ТЕСТОВЫЙ бот)

Создай **отдельного** бота у @BotFather и используй его токен. Токен боевого бота здесь не нужен: запуск тестового бота ничего не ломает, а `setWebhook` на боевом токене отключил бы Python-бота.

Программа читает переменные **из окружения** (файл `.env` она не загружает; `.env.example` — только шаблон со списком имён).

Mac (Терминал):

```
export TELEGRAM_BOT_TOKEN="токен_тестового_бота"
export WEBHOOK_SECRET="любая_длинная_строка"
export OWNER_ID="твой_telegram_id"
cargo run
```

Windows (PowerShell):

```
$env:TELEGRAM_BOT_TOKEN="токен_тестового_бота"
$env:WEBHOOK_SECRET="любая_длинная_строка"
$env:OWNER_ID="твой_telegram_id"
cargo run
```

`GROQ_API_KEY` и `SUPABASE_*` можно не задавать: тогда ИИ и мост Supabase просто отключены (в логе будет «supabase не настроен»). В логе должно появиться `бот: @имя_тестового_бота` и `слушаю порт 10000`.

В другом окне терминала (Mac; в PowerShell вместо `curl` используй `curl.exe`):

```
curl -i localhost:10000/health
curl -i -X POST localhost:10000/webhook -d '{}'
curl -i -X POST localhost:10000/webhook -H "X-Telegram-Bot-Api-Secret-Token: любая_длинная_строка" -d '{}'
```

Ожидание: `/health` → `200` и JSON; запрос без секрета → `403 forbidden`; запрос с секретом → `200 ok` (в логе предупреждение о неразобранном апдейте — это нормально для пустого `{}`).

Проверка настоящей команды `/start` требует публичного HTTPS-адреса (например, `cloudflared tunnel --url http://localhost:10000` или `ngrok http 10000`) и вызова `setWebhook` **тестового** бота с параметрами `url=<адрес>/webhook` и `secret_token=<тот же WEBHOOK_SECRET>`. Это делаем только после того, как сборка и тесты зелёные.

## 4. Если сборка упала

Пришли вывод целиком:

```
rustc --version
cargo --version
cargo build 2>&1 | tee build.log
```

(в PowerShell: `cargo build 2>&1 | Tee-Object build.log`) — достаточно первых ~100 строк файла `build.log`. Заранее известные подозрительные места (API teloxide 0.13) перечислены в `ARCHITECTURE.md`, раздел «Что проверить при первой сборке».

## 5. Чего здесь пока нет

Дополнительный код не пишем, пока каркас не скомпилирован. Redis (`amoore:data`) Rust-бот не использует: формат ключа описан в `ARCHITECTURE.md`, условия, при которых появится доступ, — там же.
