# Northgate International School - Parent AI Assistant

A production-oriented school chat website: a Rust + Axum backend, PostgreSQL
through SeaORM, a single-worker AI queue, and a dependency-free HTML/CSS/JS
frontend. It deploys as one standalone service and uses Neon branch services
for Postgres, Functions, Object Storage, and Realtime.

Parents can ask about term dates, attendance, contacts, events, and school life;
the assistant remembers each browser's conversation. There are no accounts, no
cookies, no JavaScript frameworks, and no frontend build step.

---

## Contents

- [What it does](#what-it-does)
- [Stack](#stack)
- [Requirements](#requirements)
- [Run locally](#run-locally)
- [Configuration](#configuration)
- [Neon services](#neon-services)
- [AI provider fallback chain](#ai-provider-fallback-chain)
- [School knowledge](#school-knowledge)
- [HTTP API](#http-api)
- [Database](#database)
- [Docker](#docker)
- [Deploy to Render](#deploy-to-render)
- [Logging and operations](#logging-and-operations)
- [Security notes](#security-notes)
- [Troubleshooting](#troubleshooting)
- [Project layout](#project-layout)
- [Development checks](#development-checks)

---

## What it does

- **Guest mode.** The browser creates a UUID v4, stores it in `localStorage`
  under `school_chat_user_id`, and sends it with every message. The server
  groups history by that UUID. There is no login UI.
- **Conversation memory.** Every `POST /chat` replays the guest's saved turns,
  asks the AI, and then stores the question and answer in one transaction.
- **One global queue.** A single worker processes jobs with a configurable
  cool-down between them, so provider quotas are never bursted.
- **Fallback chain.** Groq models are tried in order, then OpenRouter. Any
  provider error (429, 5xx, timeout, malformed response) moves on.
- **Fail-closed configuration.** The service refuses to start without both API
  keys, a PostgreSQL URL, the admin token, or the school knowledge file.
- **Neon branch services.** Postgres stores transcripts, a Neon Function is
  proxied over HTTPS, the `assets` bucket is managed through an S3 SigV4 client,
  and short-lived sealed Realtime capabilities follow a guest's transcript.
- **One binary, one image.** The Rust server serves the UI, the chat API, and
  the admin-only Neon diagnostics; no Node process runs in production.

## Stack

| Layer | Choice |
| --- | --- |
| HTTP | Axum 0.8 |
| Database | Neon Lakebase Postgres |
| ORM | SeaORM 1.1 with `sqlx-postgres` |
| AI | Groq and OpenRouter, OpenAI-compatible chat completions |
| Functions | Neon Functions (Node.js handler, TypeScript source) |
| Files | Neon Object Storage (S3-compatible) |
| Live data | Neon Realtime (sealed live SQL) |
| Frontend | `static/index.html` |
| IaC / typecheck | `neon.ts`, TypeScript, npm |

## Requirements

- Rust (stable) and Cargo.
- PostgreSQL 14 or newer (Neon in production).
- A Groq key (<https://console.groq.com/keys>) and an OpenRouter key
  (<https://openrouter.ai/keys>).
- Node.js 20+ and npm for `neon.ts`, the Neon CLI, and `npm run build`.
  Node is not needed at runtime.

## Run locally

1. Start PostgreSQL and create a database:

   ```bash
   createdb school_chat
   ```

   Or with Docker:

   ```bash
   docker run --name school-chat-db \
     -e POSTGRES_PASSWORD=postgres \
     -e POSTGRES_DB=school_chat \
     -p 5432:5432 \
     -d postgres:16
   ```

2. Create `.env` from `.env.example` and fill in the values:

   ```bash
   cp .env.example .env
   ```

   At minimum: `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `NEON_ADMIN_TOKEN`, and
   `DATABASE_URL`. Generate the admin token with `openssl rand -hex 32`.

3. Install the Neon tooling and build:

   ```bash
   npm install
   npm run build          # tsc --noEmit + cargo build --release
   ```

4. Start the service:

   ```bash
   npm start              # target/release/school-chat
   ```

   The schema is created automatically on first start. Open
   <http://localhost:3000>.

5. Check it:

   ```bash
   curl http://localhost:3000/health
   # {"status":"ok"}
   ```

## Configuration

All settings come from the environment. `.env` is loaded on startup for local
development; production should inject real environment variables.

| Variable | Default | Purpose |
| --- | --- | --- |
| `GROQ_API_KEY` | required | Groq API key (at least 16 characters) |
| `OPENROUTER_API_KEY` | required | OpenRouter API key (at least 16 characters) |
| `DATABASE_URL` | required | PostgreSQL URL, e.g. `postgres://user:pass@host:5432/school_chat` |
| `DATABASE_URL_UNPOOLED` | falls back to `DATABASE_URL` | Direct URL used only for schema bootstrap |
| `NEON_ADMIN_TOKEN` | required | Bearer token for `/neon/*`; at least 16 characters |
| `NEON_FUNCTION_HELLO_BASE_URL` | unset | Deployed Neon Function URL |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | unset | Branch Object Storage credentials |
| `AWS_ENDPOINT_URL_S3` / `AWS_REGION` | unset | Branch Object Storage endpoint and region |
| `NEON_STORAGE_BUCKET` | `assets` | Bucket managed by the storage endpoints |
| `NEON_REALTIME_URL` | unset | Realtime proxy WebSocket URL (`wss://`) |
| `NEON_REALTIME_SECRET` | unset | Server-only key used to seal live queries |
| `PORT` | `3000` | HTTP listen port |
| `DATABASE_MAX_CONNECTIONS` | `5` | PostgreSQL pool size (1-64) |
| `QUEUE_CAPACITY` | `8` | Chat jobs allowed to wait in the queue |
| `WORKER_INTERVAL_SECS` | `4` | Cool-down between AI jobs |
| `AI_ATTEMPT_TIMEOUT_SECS` | `30` | Timeout for one provider attempt (5-120) |
| `CHAT_RESPONSE_TIMEOUT_SECS` | `180` | Total time a request may wait (30-900) |
| `SHUTDOWN_TIMEOUT_SECS` | `25` | Grace period for the AI worker on shutdown (5-120) |
| `LOG_FORMAT` | `pretty` | `pretty` or `json` |
| `RUST_LOG` | see `.env.example` | `tracing` filter |
| `SCHOOL_KNOWLEDGE_PATH` | `school_knowledge.md` | Markdown knowledge file |
| `SCHOOL_NAME` | first `#` heading | Display name override |
| `GROQ_MODELS` | see below | Comma-separated Groq model order |
| `OPENROUTER_MODELS` | `openrouter/free` | Comma-separated OpenRouter order |
| `GROQ_API_URL` | Groq endpoint | Provider URL override |
| `OPENROUTER_API_URL` | OpenRouter endpoint | Provider URL override |
| `OPENROUTER_HTTP_REFERER` | `http://localhost:3000` | OpenRouter attribution |
| `OPENROUTER_APP_TITLE` | `School Chat` | OpenRouter attribution |

## Neon services

`neon.ts` is the infrastructure declaration for one Neon branch. Initialize and
apply it with the Neon CLI:

```bash
neon init -y --org-id <org-id> --project-id <project-id> \
  --branch production \
  --services postgres,ai-gateway,object-storage,functions,realtime
neon deploy
```

`neon deploy` applies the bucket and function, then pulls the branch variables
into `.env`. It does not overwrite provider keys that are already there. The
declared services are:

| Service | Declaration | Runtime wiring |
| --- | --- | --- |
| Postgres | Branch database | SeaORM on pooled `DATABASE_URL`; schema bootstrap on `DATABASE_URL_UNPOOLED` |
| Functions | `hello` in `hello.ts` | `GET /neon/functions/hello` proxies the deployed HTTPS URL |
| Object Storage | `assets` bucket (private) | S3 SigV4 client in `src/neon.rs`; list/put/get/delete endpoints |
| Realtime | `preview.realtime: true` | Sealed live query for a guest transcript |
| AI Gateway | `aiGateway: false` | Disabled because the Free plan does not serve gateway requests |

### Postgres

Neon exposes two URLs:

- `DATABASE_URL` is pooled (PgBouncer). The application uses it for all chat
  traffic.
- `DATABASE_URL_UNPOOLED` is direct. The server uses it once at startup to
  create `chat_messages`, then closes that connection. If the direct URL is not
  set (plain local Postgres), bootstrap falls back to `DATABASE_URL`.

### Functions

`hello.ts` is deployed as `hello` and gets a public HTTPS URL. The app reads
`NEON_FUNCTION_HELLO_BASE_URL` and exposes a bearer-protected proxy at
`GET /neon/functions/hello`, which returns the function's status and body.

### Object Storage

Object Storage is S3-compatible and branch-scoped. The app implements SigV4
signing directly in Rust (no AWS SDK) and reads the standard variables written
by `neon deploy`: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`AWS_ENDPOINT_URL_S3`, and `AWS_REGION`. The declared bucket is `assets`.

Keys are restricted to letters, digits, `/`, `-`, `_`, `.`, `~`, with no empty
or `..` segments. Uploads are capped at 5 MiB. All storage endpoints require the
admin bearer token.

### Realtime

Realtime is in beta. The server mints a **sealed live query** — a short-lived
compact JWE (AES-256-GCM) — for one guest's transcript:

```text
SELECT id, user_id, role, content, created_at
FROM chat_messages WHERE user_id = $1 ORDER BY id
```

`GET /neon/realtime/sealed?user_id=<uuid>` returns the capability, its
fingerprint, and a 60-second expiry. This is the same `@neon/realtime/server`
sealing format; `src/neon.rs` implements it in Rust so the Rust server can issue
capabilities without a Node sidecar. The browser consumes the capability with
`createRealtimeClient({ url })` and `client.subscribe(capability)`.

The endpoint is admin-only: the current guest-UUID model has no identity proof,
so exposing a transcript capability publicly would widen read access. Wire it
through an authenticated endpoint before rendering live messages in the UI.

### AI Gateway

The gateway is declared in `neon.ts` but disabled because the Neon Free plan
rejects deployments with it enabled. After upgrading, set `aiGateway: true` in
`neon.ts`, run `neon deploy`, and update the app to use the injected gateway
variables.


## AI provider fallback chain

Built-in order:

```text
Groq:       qwen/qwen3.8-27b, openai/gpt-oss-120b, openai/gpt-oss-20b
OpenRouter: openrouter/free
```

Every Groq model is attempted before OpenRouter. The first non-empty reply
wins. A failure is logged with the provider, model, attempt number, latency,
and error. Set `GROQ_MODELS` or `OPENROUTER_MODELS` to replace a chain
completely; up to ten models per provider are accepted.

## School knowledge

Answers are constrained to `school_knowledge.md`. The file is read at startup;
the service exits if it is missing or empty. Set `SCHOOL_KNOWLEDGE_PATH` to use
a different file, and `SCHOOL_NAME` to override the school name parsed from the
first `#` heading.

The system prompt tells the model to answer only from this document, to admit
when information is missing, and to direct parents to the main office.

## HTTP API

### `POST /chat`

```json
{ "user_id": "6f1c2b7e-...", "message": "When does term start?" }
```

Success:

```json
{ "status": "ok", "reply": "..." }
```

Errors use a `status` plus a human-readable `message`:

- `400` - invalid JSON, invalid UUID, empty message, or more than 4,000
  characters.
- `503` - queue full, all AI providers unavailable, or response timeout.
- `500` - internal database or worker failure.

### `GET /health`

Returns `200 {"status":"ok"}` when the worker queue is open and the database
responds to a ping within three seconds; otherwise `503`.

### Neon admin endpoints

Every `/neon/*` route requires a bearer token matching `NEON_ADMIN_TOKEN`:

```bash
# Load the same value the server uses.
export NEON_ADMIN_TOKEN="$(grep '^NEON_ADMIN_TOKEN=' .env | cut -d= -f2-)"
```

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/neon/status` | Live reachability of Postgres, Functions, Storage, and Realtime config |
| `GET` | `/neon/functions/hello` | Invoke the deployed Neon Function through the server |
| `GET` | `/neon/storage/objects?prefix=&limit=` | List bucket objects |
| `PUT` | `/neon/storage/objects/{key}` | Upload a body (5 MiB cap, `Content-Type` preserved) |
| `GET` | `/neon/storage/objects/{key}` | Download an object |
| `DELETE` | `/neon/storage/objects/{key}` | Delete an object |
| `GET` | `/neon/realtime/sealed?user_id=<uuid>` | Mint a 60-second sealed transcript query |

```bash
curl -H "Authorization: Bearer $NEON_ADMIN_TOKEN" \
  http://localhost:3000/neon/status

curl -H "Authorization: Bearer $NEON_ADMIN_TOKEN" \
  http://localhost:3000/neon/functions/hello

printf 'hello from storage' | curl -X PUT \
  -H "Authorization: Bearer $NEON_ADMIN_TOKEN" \
  -H "Content-Type: text/plain" \
  --data-binary @- \
  http://localhost:3000/neon/storage/objects/proof/hello.txt

curl -H "Authorization: Bearer $NEON_ADMIN_TOKEN" \
  "http://localhost:3000/neon/storage/objects?prefix=proof/"

curl -H "Authorization: Bearer $NEON_ADMIN_TOKEN" \
  http://localhost:3000/neon/storage/objects/proof/hello.txt

curl -H "Authorization: Bearer $NEON_ADMIN_TOKEN" \
  "http://localhost:3000/neon/realtime/sealed?user_id=6f1c2b7e-..."
```

## Database

The `chat_messages` table is created at startup from the SeaORM entity with the
PostgreSQL backend. No hand-written SQL or migration step is required. When
`DATABASE_URL_UNPOOLED` is present, this one DDL statement runs on the direct
endpoint; the long-lived pool then connects through the pooled `DATABASE_URL`.

| Column | Type | Notes |
| --- | --- | --- |
| `id` | `SERIAL` | Primary key |
| `user_id` | `TEXT` | Browser UUID, indexed for history queries |
| `role` | `TEXT` | `user` or `assistant` |
| `content` | `TEXT` | Message body |
| `created_at` | `TIMESTAMPTZ` | UTC |

`user_id` is deliberately not a foreign key, so a future accounts table can be
linked to existing guest conversations.

## Docker

```bash
docker build -t school-chat .
docker run --rm -p 3000:3000 \
  -e DATABASE_URL="postgres://user:pass@host:5432/school_chat" \
  -e NEON_ADMIN_TOKEN="..." \
  -e GROQ_API_KEY="..." \
  -e OPENROUTER_API_KEY="..." \
  school-chat
```

The image runs as a non-root user, exposes `PORT` (default `3000`), and has a
health check against `/health`.

## Deploy to Render

1. Push the repository to GitHub.
2. Create a **PostgreSQL** instance on Render and copy its internal connection
   string.
3. Create a **Web Service** from the repository:
   - Environment: **Docker**.
   - Health check path: `/health`.
4. Add environment variables:
   - `DATABASE_URL` - the Render PostgreSQL internal URL, or the pooled Neon
     URL when keeping Neon as the database.
   - `NEON_ADMIN_TOKEN` - a long random bearer token.
   - `GROQ_API_KEY`, `OPENROUTER_API_KEY`.
   - Optionally `LOG_FORMAT=json`, `RUST_LOG`, and the queue/timeout settings.
5. Deploy. Render injects `PORT`; the Dockerfile's `3000` is only the local
   fallback.

To use Neon services from Render, add the rest of the branch variables
(`DATABASE_URL_UNPOOLED`, `NEON_FUNCTION_HELLO_BASE_URL`, the four `AWS_*`
values, and `NEON_REALTIME_URL`/`NEON_REALTIME_SECRET`). Store them as Render
secret environment variables, never in the image.

PostgreSQL is a managed service, so no persistent disk or `/data` volume is
needed. The table is created on the first successful start.

## Logging and operations

- `LOG_FORMAT=pretty` for local development, `json` for log aggregators.
- Startup logs the version, port, queue size, worker interval, AI timeout, and
  school name; it never logs API keys.
- On `SIGTERM`, Ctrl+C, or Ctrl+Break, the HTTP server stops accepting requests,
  the AI worker drains queued jobs without the cool-down, and the database pool
  closes. The worker is aborted if it exceeds `SHUTDOWN_TIMEOUT_SECS`.
- Every AI attempt logs provider, model, attempt number, latency, and error.

## Security notes

- Keep `.env` out of version control; it contains API keys, the database
  password, Object Storage credentials, and the Realtime secret.
- All `/neon/*` routes are bearer-protected and compare the token in constant
  time. Use HTTPS in production.
- The chat endpoint validates the UUID and caps messages at 4,000 characters;
  chat request bodies are capped at 16 KiB. Storage uploads are capped at 5 MiB.
- Provider error bodies are truncated before logging, and keys, storage
  credentials, and the Realtime secret are never logged or returned.
- Sealed Realtime capabilities live for 60 seconds and are bound to one fixed
  transcript query and one guest UUID.
- Guest history is grouped by the browser UUID and there is no login. The
  Realtime capability endpoint remains admin-only for that reason.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `DATABASE_URL is required` | `.env` is missing or the variable is unset |
| `DATABASE_URL must be a PostgreSQL URL` | A SQLite or malformed URL was supplied |
| `database initialisation failed` | PostgreSQL is not running, unreachable, or the credentials/database are wrong |
| `GROQ_API_KEY is required` | Key missing from `.env` or the process environment |
| `could not read the school knowledge file` | `school_knowledge.md` is missing or `SCHOOL_KNOWLEDGE_PATH` is wrong |
| `503` with `busy` | Queue is full or an AI call is slow; retry shortly |
| `503` after all attempts | Every Groq and OpenRouter model failed; check provider status and keys |
| `401` from `/neon/*` | Missing/wrong `Authorization: Bearer $NEON_ADMIN_TOKEN` header |
| `Object storage is partially configured` | Some of the four `AWS_*` variables are missing; run `neon deploy` |
| `NEON_REALTIME_SECRET must contain a 32-byte key` | Stale secret; run `neon deploy` or `neon realtime rotate-secret` |
| `Object storage is not configured` | The branch has no storage variables; check `neon.ts` and `neon deploy` |
| AI Gateway warning during deploy | Expected on the Free plan; the gateway is intentionally disabled |

## Project layout

```text
.
├── Cargo.toml            # Rust dependencies
├── Dockerfile            # production image
├── .dockerignore
├── .env.example          # documented configuration
├── neon.ts               # Neon branch services (Postgres, Functions, Storage, Realtime)
├── hello.ts              # Neon Function source
├── package.json          # Neon CLI, typecheck, and build scripts
├── tsconfig.json
├── scripts/
│   └── verify-neon.mjs   # live Neon service round-trip check
├── school_knowledge.md   # grounding document
├── src/
│   ├── main.rs           # config, AI chain, queue worker, Axum handlers
│   ├── neon.rs           # Functions client, S3 SigV4 client, Realtime sealing
│   └── entity/
│       └── chat_message.rs
└── static/
    └── index.html        # single-page frontend
```

## Development checks

```bash
npm run build        # tsc --noEmit and cargo build --release
npm run check:rust   # fmt check, clippy with -D warnings, unit tests

# with the server running:
npm run verify:neon  # live Function, Storage round-trip, and Realtime subscription
```
