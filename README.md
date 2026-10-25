# Northgate International School — Parent AI Assistant

A production-oriented school chat website: a Rust + Axum backend, SQLite through
SeaORM, a single-worker AI queue, and a dependency-free HTML/CSS/JS frontend.
Parents can ask about term dates, attendance, contacts, events, and school life;
the assistant remembers each browser's conversation.

No accounts, no cookies, no JavaScript frameworks, and no frontend build step.

---

## Contents

- [What it does](#what-it-does)
- [Request lifecycle](#request-lifecycle)
- [AI provider fallback chain](#ai-provider-fallback-chain)
- [Requirements](#requirements)
- [Run locally](#run-locally)
- [Configuration](#configuration)
- [School knowledge](#school-knowledge)
- [HTTP API](#http-api)
- [Logging and operations](#logging-and-operations)
- [SQLite in production](#sqlite-in-production)
- [Docker](#docker)
- [Deploy to Render](#deploy-to-render)
- [Deploy to Shuttle](#deploy-to-shuttle)
- [Security notes](#security-notes)
- [Troubleshooting](#troubleshooting)
- [Project layout](#project-layout)
- [Development checks](#development-checks)
- [Known limitations](#known-limitations)

---

## What it does

- **Guest mode.** The browser creates a UUID v4, stores it in `localStorage`
  under `school_chat_user_id`, and sends it with every message. The server
  groups history by that UUID. There is no login UI.
- **Conversation memory.** Every `POST /chat` replays the guest's saved
  transcript plus the new question and the school knowledge base to the model.
  Reload the page, ask a follow-up, and the assistant keeps the context.
- **Groq first, OpenRouter second.** A sequential fallback chain tries three
  Groq models, then two OpenRouter models. Any refusal, rate limit, server
  error, timeout, or malformed response advances to the next attempt.
- **One-at-a-time AI queue.** All model calls go through a single worker with a
  configurable cool-down, so a burst of parents cannot hammer free-tier quotas.
  The HTTP handler waits for its job and returns a clear status.
- **Graceful failure.** If every provider attempt fails, the API returns HTTP
  503 with a user-safe JSON message. Internal details stay in the logs.
- **Production plumbing.** Strict startup validation, structured `tracing`
  logs, SQLite WAL + pool tuning, health checks, request body limits, and
  Ctrl+C/SIGTERM graceful shutdown.
- **Editable school knowledge.** `school_knowledge.md` is read once at startup
  and injected into the system prompt. Change the file, restart the service.

## Request lifecycle

1. The browser posts `{ "user_id": "<uuid>", "message": "..." }` to `/chat`.
2. Axum validates the payload: UUID shape, non-empty message, at most 4,000
   characters, request body at most 16 KiB.
3. The job is placed on a bounded `mpsc` queue (capacity `QUEUE_CAPACITY`).
   If the queue is full, the request immediately gets HTTP 503 `busy`.
4. The single worker loads that guest's history from SQLite, appends the new
   question to a system prompt containing `school_knowledge.md`, and runs the
   provider fallback chain.
5. On success, the user turn and assistant turn are inserted in one database
   transaction. The handler returns HTTP 200 `{ "status": "ok", "reply": ... }`.
6. On provider exhaustion the handler returns HTTP 503 `{ "status": "error" }`.
   On database/internal failure it returns HTTP 500. The transcript is not
   written for a failed reply, so an unanswered question never appears in
   history.

## AI provider fallback chain

Attempts run strictly in this order, one model at a time:

| # | Provider | Model |
|---|----------|-------|
| 1 | Groq | `llama-3.3-70b-versatile` |
| 2 | Groq | `meta-llama/llama-4-scout-17b-16e-instruct` |
| 3 | Groq | `llama-3.1-8b-instant` |
| 4 | OpenRouter | `meta-llama/llama-3.3-70b-instruct:free` |
| 5 | OpenRouter | `google/gemma-2-9b-it:free` |

- **Groq endpoint:** `https://api.groq.com/openai/v1/chat/completions`
- **OpenRouter endpoint:** `https://openrouter.ai/api/v1/chat/completions`
- OpenRouter requests also send `HTTP-Referer: http://localhost:3000` and
  `X-Title: School Chat`. Both are configurable for other deployments.
- Each attempt has its own timeout (`AI_ATTEMPT_TIMEOUT_SECS`, default 30s).
  Non-2xx responses, network/timeout errors, invalid JSON, empty `choices`, and
  empty content all count as a failed attempt.
- The successful provider and model, attempt number, and latency are logged.
  When every attempt fails, the 503 response is generic; the per-attempt
  reasons are logged server-side.

Provider URLs can be overridden with `GROQ_API_URL` and `OPENROUTER_API_URL`.
This is intended for local mock servers and compatible gateways, not for
bypassing the provider APIs in production.

### Model availability

Provider catalogues change. The five IDs above are the deployment's specified
defaults, but a retired model returns HTTP 404 and the chain simply advances to
the next attempt. If every attempt 404s, check the provider's current model
list and replace the order at runtime (no rebuild required):

```bash
GROQ_MODELS=model-a,model-b,model-c
OPENROUTER_MODELS=model-x:free,model-y:free
```

Each variable is a comma-separated list and replaces that provider's entire
built-in order. The service rejects empty lists, more than ten models per
provider, and model IDs containing whitespace.

During verification on 25 October 2026, the default Llama IDs and both default
OpenRouter `:free` IDs returned HTTP 404 on a test account. Two current
alternatives that answered successfully were Groq `qwen/qwen3.8-27b` and the
OpenRouter router `openrouter/free`. Treat those as examples, not permanent
defaults: confirm availability for your own account before deploying.

## Requirements

- Rust 1.85+ (the Docker image uses a current stable toolchain).
- A Groq API key: <https://console.groq.com/keys>
- An OpenRouter API key: <https://openrouter.ai/keys>
- Optional: Docker 24+ for container builds.

Both keys are **required**. The service refuses to start if either is missing
or implausibly short, and prints a startup error explaining what to set. Real
values belong in `.env` (gitignored) or the platform's secret store, never in
source control.

## Run locally

```powershell
# Windows PowerShell
Copy-Item .env.example .env
notepad .env          # set GROQ_API_KEY and OPENROUTER_API_KEY
cargo run
```

```bash
# macOS / Linux
cp .env.example .env
${EDITOR:-nano} .env  # set GROQ_API_KEY and OPENROUTER_API_KEY
cargo run
```

Open <http://localhost:3000>.

Useful checks while it runs:

```bash
curl http://localhost:3000/health
# {"status":"ok"}

curl -X POST http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{"user_id":"3fa85f64-5717-4562-b3fc-2c963f66afa6","message":"When does term start?"}'
# {"status":"ok","reply":"..."}
```

For a production-speed binary:

```bash
cargo build --release
./target/release/school-chat
```

`.env` is loaded automatically by `dotenvy`; real environment variables take
precedence over the file. The service reads `static/` and
`school_knowledge.md` relative to its working directory, so start it from the
project root (or set the paths accordingly).

## Configuration

All values live in the environment (or `.env` locally). `.env.example` lists
every supported key with safe defaults.

### Required

| Variable | Purpose |
|----------|---------|
| `GROQ_API_KEY` | Groq API key. Startup fails if missing. |
| `OPENROUTER_API_KEY` | OpenRouter API key. Startup fails if missing. |

### Server and data

| Variable | Default | Purpose |
|----------|---------|---------|
| `PORT` | `3000` | HTTP listen port. Render sets this automatically. |
| `DATABASE_URL` | `sqlite:school_chat.db` | SQLite URL. In Docker this defaults to `sqlite:/data/school_chat.db`. |
| `SQLITE_MAX_CONNECTIONS` | `5` | Pool size (1–64). SQLite still serialises writes. |
| `SCHOOL_KNOWLEDGE_PATH` | `school_knowledge.md` | Markdown file injected into the system prompt. |
| `SCHOOL_NAME` | first `# ` heading | Display name and prompt identity. |

### Queue, timeouts, and shutdown

| Variable | Default | Purpose |
|----------|---------|---------|
| `QUEUE_CAPACITY` | `8` | Number of waiting chat jobs. Full queue returns HTTP 503 `busy`. |
| `WORKER_INTERVAL_SECS` | `4` | Cool-down between AI jobs (max 3600). Higher = gentler on quotas. |
| `AI_ATTEMPT_TIMEOUT_SECS` | `30` | Per-provider/model timeout (5–120). |
| `CHAT_RESPONSE_TIMEOUT_SECS` | `180` | How long an HTTP request waits for its queued job (30–900). |
| `SHUTDOWN_TIMEOUT_SECS` | `25` | Grace period for the worker before it is aborted (5–120). |

### Logging

| Variable | Default | Purpose |
|----------|---------|---------|
| `LOG_FORMAT` | `pretty` (Docker: `json`) | `pretty` for humans, `json` for aggregators. |
| `RUST_LOG` | `info,sqlx=warn,tower_http=info,axum=info` | `tracing-subscriber` filter. |

### Provider overrides (advanced)

| Variable | Default |
|----------|---------|
| `GROQ_MODELS` | the three Groq IDs above, comma-separated |
| `OPENROUTER_MODELS` | the two OpenRouter IDs above, comma-separated |
| `GROQ_API_URL` | `https://api.groq.com/openai/v1/chat/completions` |
| `OPENROUTER_API_URL` | `https://openrouter.ai/api/v1/chat/completions` |
| `OPENROUTER_HTTP_REFERER` | `http://localhost:3000` |
| `OPENROUTER_APP_TITLE` | `School Chat` |

Set the OpenRouter referer to your real public origin when you deploy.

## School knowledge

`school_knowledge.md` is the assistant's only source of school facts. It is
read **once at startup**; unreadable or empty files abort the boot with a clear
error. To change the content:

1. Edit `school_knowledge.md`.
2. Restart the process (`cargo run`, or redeploy the container).

The generated system prompt includes the exact instruction:

> Read the provided school_knowledge.md carefully to answer the user's question.

The assistant is told to use only that information, admit when an answer is
missing, and direct parents to the main office. Plain-text answers are
requested; Markdown syntax is discouraged because the UI renders text, not
Markdown.

If the file starts with a level-1 heading, that heading becomes the school
name unless `SCHOOL_NAME` is set.

## HTTP API

### `POST /chat`

Request:

```json
{ "user_id": "3fa85f64-5717-4562-b3fc-2c963f66afa6", "message": "What time does school start?" }
```

Success `200`:

```json
{ "status": "ok", "reply": "School starts at 8:15 a.m." }
```

Status codes:

| Status | Body `status` | Meaning |
|--------|---------------|---------|
| `200` | `ok` | Reply generated and saved. |
| `400` | `error` | Invalid JSON, malformed UUID, empty message, or message too long. |
| `500` | `error` | Database or internal failure. Details are logged, not returned. |
| `503` | `busy` | Queue full or the reply exceeded `CHAT_RESPONSE_TIMEOUT_SECS`. Retry shortly. |
| `503` | `error` | Every AI provider attempt failed, or the worker is shutting down. |

All error bodies use `{ "status": "...", "message": "..." }` with a message
safe to show to a parent.

### `GET /health`

- `200 {"status":"ok"}` when SQLite responds within 3 seconds and the worker
  queue is open.
- `503 {"status":"error","message":"..."}` when the database ping fails/times
  out or the worker is unavailable.

Use `/health` as the container/platform health check. It is intentionally
cheap: no AI calls, no table scans.

## Logging and operations

`tracing` emits:

- one span per HTTP request with method, path, status, and latency;
- payload rejections and validation failures;
- every failed provider attempt with provider, model, attempt number, latency,
  and a truncated error;
- the successful provider/model and latency for each reply;
- queue-full, provider-exhaustion, database, and shutdown events.

Local development (`LOG_FORMAT=pretty`):

```text
INFO school_chat: AI provider succeeded provider="groq" model="llama-3.3-70b-versatile" attempt=1 latency_ms=842
INFO school_chat: AI reply delivered user_id=... provider=groq model=llama-3.3-70b-versatile chars=214
```

Production (`LOG_FORMAT=json`) emits one JSON object per line, suitable for
Render, Loki, CloudWatch, and similar collectors. Set `RUST_LOG=debug` only
while diagnosing; it is noisy and may include provider error bodies.

Shutdown behaviour:

- Ctrl+C (and Ctrl+Break on Windows) and, on Unix, SIGTERM stop accepting new
  connections.
- In-flight HTTP requests are allowed to finish.
- The worker drains queued jobs, then the SQLite pool is closed.
- If the worker does not stop within `SHUTDOWN_TIMEOUT_SECS`, it is aborted and
  the pool is still closed. Exit code is 0 on a clean shutdown.

## SQLite in production

The pool is configured with:

- `max_connections = SQLITE_MAX_CONNECTIONS` (default 5), `min_connections = 1`;
- 10s connect timeout, 30s acquire timeout, 5-minute idle timeout,
  30-minute max lifetime;
- WAL journal mode for file databases, `synchronous=NORMAL`, foreign keys on,
  5-second busy timeout, `create_if_missing`;
- SQLx statement logging disabled (application logs remain structured).

Operational rules:

- **Run one instance.** The AI queue and SQLite writer live in process memory.
  Do not scale horizontally against the same database file. A network-mounted
  SQLite file is not supported.
- **Persist the database.** In Docker, mount a volume at `/data` and keep
  `DATABASE_URL=sqlite:/data/school_chat.db`. Without a persistent volume,
  history disappears on redeploy/restart.
- **Back up** by copying the database while the service is stopped, or use
  `sqlite3 school_chat.db ".backup 'backup.db'"` for an online backup.
- WAL creates `-wal` and `-shm` companion files; copy/back them up together.
- SQLite serialises writes. The worker is already single-writer, so the pool is
  for read/health concurrency rather than parallel AI writes.

## Docker

The image is a multi-stage build:

- **Builder:** full Rust toolchain, dependency layer cached with a dummy
  `main.rs`, release build with fat LTO, one codegen unit, stripped symbols, and
  `panic = "abort"`.
- **Runtime:** `debian:bookworm-slim`, only `ca-certificates`, `curl`, `gosu`,
  and `passwd`; runs as unprivileged uid/gid `10001`; static files and knowledge
  copied in; `/data` prepared for SQLite.
- The entrypoint fixes ownership on a mounted `/data` volume, then drops from
  root to `app` with `gosu`.
- Health check: `curl --fail http://127.0.0.1:${PORT}/health`.
- `STOPSIGNAL SIGTERM`, JSON logs by default.

Build and run:

```bash
docker build -t school-chat .

docker run --rm -p 3000:3000 \
  --env-file .env \
  -e DATABASE_URL=sqlite:/data/school_chat.db \
  -v school-chat-data:/data \
  school-chat
```

`-e DATABASE_URL=...` is important: a local `.env` usually contains
`DATABASE_URL=sqlite:school_chat.db`, and `--env-file` would otherwise override
the image default and write inside the container layer. With the explicit
override, the database lives on the named volume.

Do not bake keys into the image. Pass them at runtime with `--env-file`, `-e`,
or your platform's secret store. `.dockerignore` excludes `.env`, databases,
`target/`, and documentation.

## Deploy to Render

1. Push the repository to GitHub.
2. Create a **Web Service** → **Docker**; use the repository root as the build
   context (the `Dockerfile` is detected automatically).
3. Set the health check path to `/health`.
4. Add environment variables:
   - `GROQ_API_KEY`, `OPENROUTER_API_KEY` (mark as secrets);
   - `DATABASE_URL=sqlite:/data/school_chat.db`;
   - `LOG_FORMAT=json`;
   - optionally `RUST_LOG`, `QUEUE_CAPACITY`, `WORKER_INTERVAL_SECS`, and the
     timeout values;
   - `OPENROUTER_HTTP_REFERER=https://your-service.onrender.com`.
5. Attach a **persistent disk** mounted at `/data`. Persistent disks are
   available on paid instance types; choose one instance, not autoscaling.
6. Deploy. Render sets `PORT`; do not hardcode a different port.

Render notes:

- Without a persistent disk (for example, the free tier), the container
  filesystem is ephemeral: chat history is lost on restart or redeploy. Treat
  free-tier deployments as demos.
- Keep the service at **one instance**. Multiple instances would each have
  their own in-memory queue and would contend for the same SQLite file.
- The platform sends SIGTERM on deploy; the app drains in-flight work for up to
  `SHUTDOWN_TIMEOUT_SECS`.
- If the platform has a slow cold start, raise the health check grace period.
  The app binds only after the knowledge file, configuration, and database are
  ready.

## Deploy to Shuttle

Shuttle builds Rust projects from source; it does not consume this Dockerfile
the way a container platform does. For a Shuttle deployment:

- Create `Shuttle.toml` with the project name and any Shuttle-specific
  configuration.
- Store `GROQ_API_KEY` and `OPENROUTER_API_KEY` in Shuttle's secret store, not
  in source or `Shuttle.toml`.
- Set `DATABASE_URL` to a writable location that survives deploys. If your
  Shuttle plan/feature set does not provide persistent storage, point the app
  at a persistent database instead of relying on the container filesystem, or
  adapt the data layer to Postgres.
- Use the same `PORT` and `/health` conventions; the app binds `0.0.0.0`.
- `school_knowledge.md`, `static/`, and the binary must be present in the
  deployed project.

The Dockerfile remains the reference for a reproducible container build and is
useful for Render, Fly.io, Railway, or any Docker host.

## Security notes

- **Never commit real keys.** `.env` is gitignored, `.dockerignore` excludes it,
  and no key is compiled into the binary or baked into the image.
- Keys are read from the process environment and validated at startup. Startup
  errors name the missing variable and where to obtain it; they never print the
  key value.
- Provider error bodies are truncated before logging. Chat payloads are not
  logged in full; only the guest UUID and message length are logged.
- The database stores guest UUIDs and chat text. There is no authentication;
  anyone with a UUID can read that UUID's history by using it in requests. Do
  not treat the UUID as a security boundary for sensitive data.
- The app does not parse `X-Forwarded-*` headers. If you add a reverse proxy,
  terminate TLS there, forward only to the app's private port, and set
  `OPENROUTER_HTTP_REFERER` to the public origin.
- Rotate any key that has been pasted into a chat, ticket, or log. Use separate
  keys for local development and production.
- Set a real `OPENROUTER_HTTP_REFERER` and `OPENROUTER_APP_TITLE` in production;
  the localhost default is for development only.

## Troubleshooting

| Symptom | Cause and fix |
|---------|---------------|
| `startup configuration error: GROQ_API_KEY is required` | Set the key in `.env` or the environment. Both provider keys are mandatory. |
| `startup configuration error: ... too short` | A placeholder or truncated key was supplied. Use the full provider key. |
| `503 {"status":"busy"}` | The queue is full or a reply exceeded `CHAT_RESPONSE_TIMEOUT_SECS`. Wait a few seconds and retry; raise `QUEUE_CAPACITY` or lower `WORKER_INTERVAL_SECS` only if quotas allow. |
| `503 {"status":"error"}` | Every Groq and OpenRouter attempt failed. Check the logs for per-model HTTP status/timeouts, provider status pages, and key quotas. |
| `500 {"status":"error"}` | Database or internal failure. Check the log entry with the same request ID/timestamp; verify `/data` is writable and the disk is not full. |
| Health check returns 503 | SQLite ping failed/timed out, or the worker is unavailable. Check the volume mount and `/data` permissions. |
| `database is locked` | A second process or another replica is writing the same SQLite file, or a long transaction is blocking. Run one instance and keep writes short. |
| Knowledge changes are ignored | The file is read once at startup. Restart the process. |
| Models return 404/429 | A model was retired or the account is rate-limited. Set `GROQ_MODELS` / `OPENROUTER_MODELS` to IDs your account currently exposes, or wait for quota reset. The startup defaults are listed in the provider-chain section. |
| Docker container exits immediately | Check `docker logs`. Common causes: missing keys, a mounted `/data` without write permission, or a missing `school_knowledge.md`. |

## Project layout

```text
.
├── Cargo.toml
├── Cargo.lock
├── .env.example              # safe template; no real secrets
├── .gitignore                # ignores .env and SQLite files
├── .dockerignore
├── Dockerfile                # multi-stage production image
├── docker-entrypoint.sh      # /data ownership fix + gosu privilege drop
├── school_knowledge.md       # the only school data source
├── static/
│   └── index.html            # single-file UI (HTML/CSS/JS)
└── src/
    ├── main.rs               # config, providers, queue, handlers, startup
    └── entity/
        └── chat_message.rs   # SeaORM entity
```

## Development checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo build --release
```

There is no frontend build step. To test provider fallback deterministically,
point `GROQ_API_URL` and `OPENROUTER_API_URL` at a local OpenAI-compatible mock
and use short timeouts; the chain order and headers are ordinary HTTP, so the
mock can assert them.

## Known limitations

- One process, one worker, one SQLite file. This is deliberate for a small
  school site and free-tier quotas, not a horizontally scalable design.
- The queue is in memory: jobs are lost on restart, and a client must retry.
- No authentication or rate limiting per guest beyond the global queue and body
  limit. Put the service behind a platform proxy or add a rate limiter before
  exposing it to a large audience.
- Chat history grows without pruning. Add a retention job if the database size
  becomes a concern.
- The built-in model lists live in `src/main.rs` and are compile-time defaults.
  Provider catalogues change; use the `GROQ_MODELS` / `OPENROUTER_MODELS`
  environment overrides instead of rebuilding when a model is retired.
- SQLite is not a multi-region or highly available store. For larger
  deployments, move the data layer to Postgres.
