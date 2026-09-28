---
name: openproxy-video
description: Generate video through OpenProxy's async `/v1/videos/*` routes — submit a text-to-video or image-to-video job, poll the returned `request_id` to completion, and download the MP4. Covers the xAI / OpenRouter / Vertex video providers, the per-account cooldown and rotation rules, and the `openproxy media video` CLI that drives the whole flow. Use whenever the user asks to create a video, animate an image, wire a video model into OpenProxy, or debug a stuck or failed video job.
---

# openproxy-video — async video generation

Video is the only OpenProxy endpoint family that is **two-phase**. `POST /v1/videos/generations` returns a job id in ~1 second; the MP4 does not exist yet. You must `GET /v1/videos/{id}` until the job reaches a terminal state, then fetch the file from the URL in the finished payload. Treating the create response as the video is the single most common mistake.

Video routes live in `src/server/api/media.rs` and are registered in `src/server/api/mod.rs:220-261`.

## 0 · The five routes that exist

| Method | Path | Handler | Notes |
|---|---|---|---|
| `POST` | `/v1/videos/generations` | `media::video_generations` | Text-to-video and image-to-video. |
| `POST` | `/v1/videos/edits` | `media::video_edits` | xAI Grok Imagine. Multipart-capable. |
| `POST` | `/v1/videos/extensions` | `media::video_extensions` | xAI Grok Imagine. Multipart-capable. |
| `GET` | `/v1/videos/{id}` | `media::video_get` | Poll. Not a download route. |
| `POST` | `/v1/video/generations` | `media::video_generations` | Legacy **singular** spelling, kept for older clients. |

Each is also registered under a doubled prefix — `/v1/v1/videos/generations`, `/v1/v1/videos/edits`, `/v1/v1/videos/extensions`, `/v1/v1/videos/{id}`, `/v1/v1/video/generations` — so a client that already appends `/v1` to its base URL still resolves. There is **no** `GET /v1/videos` list route and **no** content-download route. To get the bytes, read the URL out of the finished poll payload and download it yourself.

All five accept `OPTIONS` preflight.

## 1 · Which providers actually serve video

Exactly three, enforced by `video_provider_supported` (`src/server/api/media.rs:2408`):

| Provider key | Adapter | Upstream | Catalog models (`kind: "video"`) |
|---|---|---|---|
| `xai` | native (default shape) | `https://api.x.ai/v1/videos/{action}` | `grok-imagine-video` |
| `openrouter` | collection-root POST | `https://openrouter.ai/api/v1/videos` | none built in — add a custom model |
| `vertex` | translated to `predictLongRunning` | `https://aiplatform.googleapis.com` | `veo-3.1-generate-preview`, `veo-3.1-fast-generate-preview`, `veo-3.0-generate-001`, `veo-2.0-generate-001` |

Model selection is read off the `model` field:

- `xai/grok-imagine-video` → provider `xai`, upstream model `grok-imagine-video` (the prefix is stripped before forwarding).
- A bare id with no `provider/` prefix → falls back to **`xai`**.
- A model whose provider is not one of the three → `400 Provider '<provider>' does not support video generation`.
- A **combo** → `400 Combos are not supported for video generation`. Video is one of the route kinds that deliberately does not expand combos — see `combo_expands` at `media.rs:755`, which covers only `audio/speech` and `images/generations`.

## 2 · Connect an account first

Video selects from the ordinary provider connections (`provider_connections` where `provider` is `xai` / `openrouter` / `vertex`, active, holding credentials, not in cooldown). It does **not** read the `/api/media-providers` registry — adding a media provider of some other kind does nothing for these routes.

```bash
export OPENPROXY_API_KEY=...   # the server API key
export OPENPROXY_URL=http://127.0.0.1:4623

openproxy key add video-agent --auto          # → openproxy.v1.key.add envelope
openproxy --robot provider apply --from-file - <<'JSON'
{
  "providers": [
    { "name": "xai-video", "provider": "xai", "apiKey": "xai-...", "isActive": true },
    {
      "name": "vertex-video",
      "provider": "vertex",
      "isActive": true,
      "providerSpecificData": { "projectId": "my-gcp-project", "location": "us-central1" }
    }
  ]
}
JSON
```

Vertex is the one that needs more than a key. The stored credential is **Service Account JSON** (or an OAuth access token) — a plain API key cannot mint a Vertex token, and the code builds Vertex headers from scratch rather than reusing the bearer path. Project id resolves in order: Service Account `project_id` → the connection's `project_id` → `providerSpecificData.projectId`. Location defaults to `us-central1`.

Confirm the account is visible to the video path before spending anything:

```bash
curl -sS "$OPENPROXY_URL/v1/videos/anything" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"
# 400 "No credentials for provider: xai"  → no account configured
# 400/5xx from the provider               → account found, upstream reachable
```

## 3 · Submit the job

```bash
curl -sS -D /tmp/op-video-headers.txt -X POST \
  "$OPENPROXY_URL/v1/videos/generations" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -H 'Idempotency-Key: my-unique-key-001' \
  -d '{"model":"xai/grok-imagine-video","prompt":"a paper boat drifting down a rain gutter at dusk","duration":6,"aspect_ratio":"16:9","resolution":"720p"}'
```

Response — the whole thing, no envelope:

```json
{"request_id":"abc-123"}
```

Two things to keep:

1. **Save the response headers.** They carry `x-openproxy-connection-id` and `x-9router-connection-id` — the account that accepted the job.
2. **Send an `Idempotency-Key`.** It is forwarded on every create attempt, including the post-token-refresh retry, so a retried create cannot bill twice. The gateway never generates one for you.

Body fields (OpenAI-ish): `model`, `prompt`, and optionally `duration`, `aspect_ratio`, `resolution`, `n`, `seed`, `negative_prompt`, plus `image` (an object, e.g. `{"url": "..."}`) or `image_url` (a string) for image-to-video. Anything else you send is passed through to the provider untouched — when the model needs no rewriting, the request body goes upstream byte for byte.

`n` and `duration` accept a number or a numeric string. `seed` is special: a numeric `0` is forwarded, because it is a legitimate "make this reproducible" value.

### Image-to-video

```bash
curl -sS -X POST "$OPENPROXY_URL/v1/videos/generations" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"vertex/veo-3.1-generate-preview","prompt":"the camera slowly pushes in","image_url":"gs://my-bucket/frame.png"}'
```

`/v1/videos/edits` and `/v1/videos/extensions` take the same body and additionally accept `multipart/form-data` — the handler branches on `Content-Type` alone, never on which action you hit, and non-JSON bodies are forwarded byte-for-byte (re-encoding a form body would change the multipart boundary). Limits: 32 MiB for the JSON arm, 512 MiB for the raw arm. Note the raw passthrough path is xAI-only; a multipart body is never routed by a `provider/` prefix.

### Vertex body translation

OpenRouter and xAI take your JSON as written. Vertex is translated to the `predictLongRunning` shape before the call, and a `GET` on its job is actually a `POST` to `{operationName}:fetchPredictOperation`:

| You send | Vertex receives |
|---|---|
| `prompt` | `instances[].prompt` |
| `image` / `image_url` (string) | `instances[].image.gcsUri` |
| `image` / `image_url` (`data:…;base64,…`) | `instances[].image.{bytesBase64Encoded,mimeType}` |
| `n` | `parameters.sampleCount` |
| `duration` | `parameters.durationSeconds` |
| `aspect_ratio` | `parameters.aspectRatio` |
| `resolution` | `parameters.resolution` |
| `seed` | `parameters.seed` |
| `negative_prompt` | `parameters.negativePrompt` |

Two Vertex guards fire before any billable call: the model id must be alphanumeric/`.`/`_`/`-` only (anything with `/` or `..` is rejected — it would rewrite the request URL), and the body must carry a `prompt` or a **present** `image`/`image_url`. `"image": null` and `"image": ""` do not satisfy that guard. Vertex and OpenRouter both accept `generations` only; posting to `/v1/videos/edits` with either one returns `400`.

## 4 · Poll until terminal

Video jobs are **account-bound upstream**. Always pin the creating account, or the provider will answer `unknown request_id` when your poll lands on a different credential.

```bash
CONN=$(grep -i '^x-openproxy-connection-id' /tmp/op-video-headers.txt | tr -d '\r' | awk '{print $2}')

curl -sS "$OPENPROXY_URL/v1/videos/abc-123" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H "x-connection-id: $CONN"
```

All three spellings are accepted on the way in: `x-connection-id`, `x-openproxy-connection-id`, `x-9router-connection-id`. If you have no connection id, add `?provider=xai|openrouter|vertex`; otherwise the poll falls back to the `xai` default.

The poll response is the provider's own payload. Read these fields:

- `status` — `done` / `completed` means finished; `failed` / `error` / `expired` / `cancelled` means it will never produce a file; anything else (`pending`, `processing`, …) means keep polling.
- `progress` — a percentage on some providers. A ten-minute job with no output reads as a hang, so surface it.
- `video.url`, or `video.file_output.public_url` — the file to download, **only present once terminal**.

Vertex poll responses are synthesized into the same shape rather than passed through: pending becomes `{"id":…,"request_id":…,"status":"pending"}`, failure adds `"error": …`, and a completed operation fills in `video.url` from `response.videos[].gcsUri`.

Treat `429` and `503` on a poll as "try again". Any other non-2xx will not get better — stop.

## 5 · Download the MP4

There is no gateway download route. Fetch the provider URL directly:

```bash
URL=$(curl -sS "$OPENPROXY_URL/v1/videos/abc-123" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H "x-connection-id: $CONN" \
  | jq -r '.video.url // .video.file_output.public_url')

curl -fsSL "$URL" -o video.mp4.part && mv video.mp4.part video.mp4
```

Write to a `.part` and rename. A player opened on a still-downloading file sees a truncated stream and no player recovers from that.

## 6 · Or let the CLI do all of it

`openproxy media video` implements exactly the flow above — create, pin, poll, download — and is the recommended path when a human is watching.

```bash
export OPENPROXY_API_KEY=...
openproxy media video \
  --prompt "a paper boat drifting down a rain gutter at dusk" \
  --model xai/grok-imagine-video \
  --duration 6 \
  --aspect-ratio 16:9 \
  --resolution 720p \
  --output boat.mp4
```

Image-to-video — a local path is read and inlined as a base64 data URL (`.png` → `image/png`, `.webp` → `image/webp`, anything else → `image/jpeg`); an `http(s)://` or `data:` URL passes through:

```bash
openproxy media video --prompt "the camera slowly pushes in" --image ./frame.png -o push-in.mp4
```

Agent mode:

```bash
openproxy --robot media video --prompt "…" -o out.mp4
# → {"schema":"openproxy.v1.media.video","ok":true,
#    "data":{"request_id":"abc-123","output":"out.mp4"}}
```

| Flag | Default | Meaning |
|---|---|---|
| `--prompt <TEXT>` | *required* | Video description. |
| `-o, --output <PATH>` | `video.mp4` | Where the finished MP4 lands. |
| `--model <ID>` | `xai/grok-imagine-video` | Model id, `<provider>/<model>` form. |
| `--duration <N>` | — | Clip length in seconds. |
| `--aspect-ratio <R>` | — | e.g. `16:9`, `9:16`, `1:1`. |
| `--resolution <R>` | — | `480p`, `720p`, `1080p`. |
| `--image <PATH\|URL>` | — | First frame, for image-to-video. |
| `--timeout <SEC>` | `600` | Give up on the job after this long. |
| `--poll-interval-ms <MS>` | `5000` | Delay between polls. |
| `--host <H>` / `--port <P>` | resolved runtime | Override the target gateway. Standalone defaults are `127.0.0.1:4623`. |

The CLI does **not** read a key out of the local database. It uses `--api-key`, else `$OPENPROXY_API_KEY`, else the active profile's `api_key_env` / `api_key`. Exported as `OPENPROXY_API_KEY`, it is used for both `x-api-key` and `Authorization: Bearer`.

Behaviour worth knowing: `Ctrl-C` unlinks `<output>.part` and exits `130`; the download is streamed to `.part` and renamed on success; `--timeout` bounds the whole job, not any single HTTP call.

## 7 · Account rotation, cooldowns, and why a job can be stuck

Video has its own rotation and lock rules, separate from the chat pipeline.

**Rotation on create** happens only on `401`, `403`, and `429` — the statuses an upstream returns *before* the job is created. A `5xx` is returned to the caller untouched. On `401`/`403` with a refresh token, the gateway refreshes, persists the new token, and re-sends the identical body exactly once, preserving the `Idempotency-Key`. `openproxy media video` mirrors this by pinning the first account on every poll.

**Cooldowns after a failure**, keyed per model rather than account-wide, so one bad prompt does not take a healthy credential out of every other model's rotation:

| Upstream status | Cooldown |
|---|---|
| `401` | 900 s |
| `403` | 900 s |
| `429` | 120 s (and bumps the account's backoff level) |
| `503` | 120 s |
| any other failure | 300 s |

A `2xx` clears both the lock and the error state, so the account comes straight back.

**When nothing is selectable**, the gateway distinguishes the two cases:

- No account configured for the provider at all → `400 No credentials for provider: <provider>`.
- Every account is cooling down → the last observed status (or `503` if nothing was attempted), message `[<provider>/video] <last error>`, and a `Retry-After` header in seconds. **Honor `Retry-After` and do not retry sooner** — hammering a locked video account is the failure mode the lock exists to prevent.

**Per-request deadline**: `VIDEO_FETCH_TIMEOUT_MS` (default `120000`) bounds each individual upstream HTTP call, not the job. Exceeding it is `408`; a refused connection is `502`. If a video job consistently times out at 120 s, raise this — but confirm the upstream is slow rather than your proxy path first.

## 8 · Error shapes

Errors use the standard OpenAI error envelope:

```json
{"error":{"code":"…","message":"…","type":"…"}}
```

Upstream error bodies are forwarded with a `[provider] ` prefix, truncated to 2000 characters, and scrubbed — `Bearer <token>` and any stored access / refresh / API key are redacted before the text reaches you. Seeing a redacted secret in an error means the credential is being sent correctly; it is not a leak.

| Status | Cause |
|---|---|
| `400` | `Combos are not supported for video generation` |
| `400` | `Provider '<p>' does not support video generation` |
| `400` | `No credentials for provider: <p>` |
| `400` | `OpenRouter video requires an application/json body` |
| `400` | `OpenRouter video supports 'generations' only` / `Vertex video supports 'generations' only` |
| `400` | `Vertex video requires a prompt or an image` / `Invalid Vertex video model id` |
| `401` | Missing or unknown `OPENPROXY_API_KEY` on the gateway |
| `408` | Upstream exceeded `VIDEO_FETCH_TIMEOUT_MS` |
| `502` | Upstream refused or reset the connection |
| `503` | Every account cooling down; read `Retry-After` |

## 9 · Auth model

The video routes gate on `require_api_key_with_reload`, and only when `settings.require_api_key()` is on. They deliberately do **not** gate on dashboard login — locking the dashboard must not lock every API client, and the usual headless posture (dashboard open) must not silently remove API auth from `/v1/*`.

Present the key either way; both are read:

```bash
-H "Authorization: Bearer $OPENPROXY_API_KEY"
-H "x-api-key: $OPENPROXY_API_KEY"
```

A `401 Invalid API key` right after minting a key usually means a stale server snapshot — the auth path reloads once from SQLite and retries, so `openproxy server stop && openproxy server start --detach --no-open` clears it if it persists.

## 10 · Verify an end-to-end job

```bash
# 1. Create and capture both the id and the account
curl -sS -D /tmp/h.txt -X POST "$OPENPROXY_URL/v1/videos/generations" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -H "Idempotency-Key: verify-$(date +%s)" \
  -d '{"model":"xai/grok-imagine-video","prompt":"a red ball rolling down a hill"}' \
  -o /tmp/create.json
cat /tmp/create.json
CONN=$(grep -i '^x-openproxy-connection-id' /tmp/h.txt | tr -d '\r' | awk '{print $2}')
RID=$(jq -r '.request_id' /tmp/create.json)

# 2. Poll, pinned to the creating account
until S=$(curl -sS "$OPENPROXY_URL/v1/videos/$RID" \
    -H "Authorization: Bearer $OPENPROXY_API_KEY" -H "x-connection-id: $CONN" \
    | jq -r '.status'); do sleep 5; done
echo "status=$S"

# 3. Download only once terminal
[ "$S" = done ] || [ "$S" = completed ] || exit 1
URL=$(curl -sS "$OPENPROXY_URL/v1/videos/$RID" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H "x-connection-id: $CONN" \
  | jq -r '.video.url // .video.file_output.public_url')
curl -fsSL "$URL" -o verify.mp4.part && mv verify.mp4.part verify.mp4
ls -l verify.mp4
```

Or the same check in one command:

```bash
openproxy --robot media video --prompt "a red ball rolling down a hill" -o verify.mp4
# → data.request_id present, data.output written; a non-zero exit means it never finished
```

## Common failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `No credentials for provider: xai` | No `xai` connection is active | `openproxy --robot provider list`, or dashboard → Providers |
| `Provider 'openai' does not support video generation` | OpenAI is not a video provider here | Use `xai/`, `openrouter/`, or `vertex/` |
| `Combos are not supported for video generation` | Model resolved to a combo | Point at a concrete `<provider>/<model>` |
| `unknown request_id` on every poll | Poll landed on a different account | Echo `x-connection-id` from the create response |
| Job stuck `pending` for minutes | Upstream is genuinely slow, or the account was rotated out | Check `Retry-After`; raise `VIDEO_FETCH_TIMEOUT_MS` if single calls are hitting 408 |
| `429` immediately on a fresh job | Every account is in cooldown | Read `Retry-After` and wait it out — do not retry sooner |
| `Vertex video requires a project_id` | Credential is not Service Account JSON, no project set | Set `providerSpecificData.projectId` or store the SA JSON |
| `Vertex video requires a prompt or an image` | Body had only `"image": null` / `""` | Send a real prompt, or a real image URL |
| OpenRouter returns `400` on create | Body was not `application/json` | OpenRouter has no multipart arm — send JSON |
| A `Bearer [redacted]` string appears in an error | Working as designed | Secrets are scrubbed from every client-bound video error |

## Environment variables

| Var | Default | Purpose |
|---|---|---|
| `VIDEO_FETCH_TIMEOUT_MS` | `120000` | Per-request upstream deadline. Bounds the HTTP call, not the job. |
| `OPENPROXY_XAI_VIDEO_URL` | `https://api.x.ai/v1/videos` | Redirect the xAI video upstream. |
| `OPENPROXY_API_KEY` | _(unset)_ | Bearer the CLI sends to the gateway. |
| `OPENPROXY_URL` | `http://127.0.0.1:4623` | Gateway the CLI targets. |
| `require_api_key` (setting, not env) | `false` | When enabled, `/v1/videos/*` rejects unauthenticated callers. There is no `REQUIRE_API_KEY` env var — no code reads it. |

## When _not_ to use this skill

- Chat, image, TTS, STT, or embedding generation — use the other endpoint skills.
- Provider catalog sync or CLI-tool wiring — see [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md).
- Combo fallback and model resolution across the chat pipeline — see `.agents/skills/openproxy-combos/SKILL.md`.

## See also

- Implementation: `src/server/api/media.rs`, routes registered at `src/server/api/mod.rs:220-261`
- CLI: `src/cli/media.rs` (`openproxy media video`)
- Catalog: `src/core/model/provider_catalog.json` (entries with `"kind": "video"`)
- Intentional divergences: `docs/parity-9router.md` — notably, a video create is **not** re-fired at the next account on a network error; it returns `502` immediately, because a dropped connection may still have billed upstream.
- Upstream docs: [xAI videos](https://docs.x.ai/developers/rest-api-reference/inference/videos) · [OpenRouter videos](https://openrouter.ai/docs/api/api-reference/videos) · [Vertex Veo](https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/veo-video-generation)
