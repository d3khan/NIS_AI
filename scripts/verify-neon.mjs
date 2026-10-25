// End-to-end check for the Neon services wired into the Rust server.
//
// Usage (with the server running):
//   node --env-file=.env scripts/verify-neon.mjs
//
// Optional: BASE_URL (default http://127.0.0.1:3000), USER_ID.

import { randomUUID } from "node:crypto";
import { createRealtimeClient } from "@neon/realtime";

const baseUrl = (process.env.BASE_URL ?? "http://127.0.0.1:3000").replace(/\/$/, "");
const adminToken = process.env.NEON_ADMIN_TOKEN;
if (!adminToken) {
  throw new Error("NEON_ADMIN_TOKEN is required");
}

process.on("unhandledRejection", (error) => {
  console.error(error);
  process.exit(1);
});
const userId = process.env.USER_ID ?? randomUUID();
const authHeaders = { Authorization: `Bearer ${adminToken}` };

async function request(path, init = {}) {
  const response = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: { ...authHeaders, ...(init.headers ?? {}) },
  });
  return response;
}

async function json(path, init) {
  const response = await request(path, init);
  const text = await response.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  if (!response.ok) {
    throw new Error(`${init?.method ?? "GET"} ${path} -> ${response.status}: ${text}`);
  }
  return body;
}

async function waitForLive(subscription, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const state = subscription.getState();
    if (state.status === "live") return;
    if (state.status === "error") {
      throw new Error(`Realtime subscription failed: ${state.error.message}`);
    }
    if (Date.now() > deadline) {
      throw new Error(`Realtime subscription did not go live: ${state.status}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

const report = {};
report.user_id = userId;

report.status = await json("/neon/status");
report.function = await json("/neon/functions/hello");

// Object Storage round-trip, including a body larger than the chat API's
// 16 KiB limit to prove the storage route has its own limit.
const key = `verification/${Date.now()}-${userId.slice(0, 8)}.txt`;
const payload = "neon-storage-ok\n".repeat(4096);
const uploaded = await json(`/neon/storage/objects/${key}`, {
  method: "PUT",
  headers: { "Content-Type": "text/plain; charset=utf-8" },
  body: payload,
});
report.storage_upload = uploaded;

const listed = await json(
  `/neon/storage/objects?prefix=${encodeURIComponent(key.slice(0, key.lastIndexOf("/") + 1))}`,
);
report.storage_list = {
  count: listed.count,
  found: listed.objects.some((object) => object.key === key),
};

const download = await request(`/neon/storage/objects/${key}`);
const downloaded = await download.text();
report.storage_download = {
  status: download.status,
  matches: downloaded === payload,
  bytes: downloaded.length,
};

report.storage_delete = await json(`/neon/storage/objects/${key}`, { method: "DELETE" });
const afterDelete = await request(`/neon/storage/objects/${key}`);
report.storage_after_delete = { status: afterDelete.status };

// Realtime: subscribe with the capability sealed by the Rust server, ask a
// question, and wait for the persisted assistant row to arrive live.
const sealed = await json(
  `/neon/realtime/sealed?user_id=${encodeURIComponent(userId)}`,
);
const client = createRealtimeClient({ url: sealed.url });
const subscription = client.subscribe(sealed);
try {
  await waitForLive(subscription);
  await subscription.awaitRows((rows) => rows.length === 0, 15_000);

  const chat = await json("/chat", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      user_id: userId,
      message: "When does Term 2 start?",
    }),
  });
  report.chat = chat;

  await subscription.awaitRows(
    (rows) => rows.some((row) => row.role === "assistant"),
    90_000,
  );
  const rows = subscription.getSnapshot().data ?? [];
  report.realtime = {
    capability_expires_at: sealed.expires_at,
    rows: rows.length,
    roles: rows.map((row) => row.role),
    assistant_preview: rows.find((row) => row.role === "assistant")?.content?.slice(0, 80),
  };
} finally {
  subscription.unsubscribe();
  client.close();
}

console.log(JSON.stringify(report, null, 2));
process.exit(0);
