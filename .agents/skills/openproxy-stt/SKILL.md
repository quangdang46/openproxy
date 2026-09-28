---
name: openproxy-stt
description: Transcribe audio through OpenProxy's OpenAI-compatible POST /v1/audio/transcriptions endpoint — multipart and base64-JSON request shapes, the seven STT providers it dispatches to, per-provider credential fallback, and the `openproxy media stt transcribe` CLI. Use whenever the user needs speech-to-text, Whisper, Deepgram, AssemblyAI, or Gemini audio transcription through the local router on 127.0.0.1:4623.
---

# openproxy — speech-to-text

OpenProxy transcribes audio on one OpenAI-compatible endpoint and routes it to whichever
credentialed STT provider owns the requested model. This skill covers:

1. Probing the server and confirming the route exists
2. Registering an STT provider credential
3. Transcribing over `POST /v1/audio/transcriptions` (multipart and JSON shapes)
4. Transcribing from the CLI
5. Reading the per-provider response shapes
6. Verifying the whole thing end to end

Everything here is non-interactive and copy-pasteable.

## 0 · Detect what's already running

Do this before mutating anything:

```bash
command -v openproxy && openproxy --version || echo "not installed"
openproxy --robot server status 2>/dev/null || echo "server not running"
curl -sS http://127.0.0.1:4623/health
```

If the server is not up, start it detached and headless:

```bash
openproxy server start --detach --no-open
curl -sS http://127.0.0.1:4623/v1 | jq .endpoints
```

`/v1` advertises `/v1/audio/transcriptions` in its endpoint list. If it is not there, the
binary is older than this skill — upgrade before continuing.

Capture the admin key once and reuse it. `/v1/*` is gated on `settings.requireLogin`,
which defaults to on, so the bearer is required unless the user has deliberately turned it off:

```bash
openproxy key add <NAME> --auto
# Or pass your own secret positionally: openproxy key add <NAME> '<SECRET>'
# If no key exists yet, capture the one-shot admin key from:
openproxy --robot server init | jq -r '.data.admin_key.key'
# Or recover the dashboard password for a running server:
openproxy auth reset-password --show
```

The bearer may be the admin key from `server init` or any proxy key minted with
`openproxy key add`. Both work on `/v1/audio/transcriptions`.

## 1 · Register an STT provider

STT providers are provider connections tagged with `mediaType: "stt"`. Add one through the
media-providers API, which encrypts the key on the way into the database:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/media-providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
        "name": "openai-stt",
        "provider": "openai",
        "apiKey": "sk-...",
        "mediaType": "stt"
      }'
```

The same call from the CLI:

```bash
openproxy media providers add --provider openai --kind stt --name openai-stt \
  --from-file - <<'JSON'
{"apiKey": "sk-..."}
JSON
```

- `--kind` accepts `stt`; it is written to the connection as `mediaType`.
- Re-adding the same `provider` + `name` **replaces** the existing row rather than creating a
  second one. Use a distinct `--name` to keep two keys for the same provider.

List and verify:

```bash
openproxy media providers list --kind stt
openproxy --robot media providers list --kind stt
```

Discover which STT models the catalog knows about:

```bash
curl -sS http://127.0.0.1:4623/v1/models/stt \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"
```

## 2 · Which providers serve STT

Seven providers have a built-in STT config in `stt_config()` (`src/server/api/stt.rs`).
Anything else returns **400 `Provider '<x>' does not support STT`**.

| Provider key | Upstream | Auth header | Wire format | Default model ids |
|---|---|---|---|---|
| `openai` | `https://api.openai.com/v1/audio/transcriptions` | `Authorization: Bearer` | OpenAI multipart | `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe` |
| `groq` | `https://api.groq.com/openai/v1/audio/transcriptions` | `Authorization: Bearer` | OpenAI multipart | `whisper-large-v3`, `whisper-large-v3-turbo`, `distil-whisper-large-v3-en` |
| `deepgram` | `https://api.deepgram.com/v1/listen` | `Authorization: Token` | raw body + query params | `nova-3`, `nova-2`, `whisper-large` |
| `assemblyai` | `https://api.assemblyai.com/v2/transcript` | `Authorization: Bearer` | upload → submit → poll | `universal-3-pro`, `universal-2` |
| `huggingface` | `https://api-inference.huggingface.co/models` | `Authorization: Bearer` | raw body to `{baseUrl}/{model}` | `openai/whisper-large-v3`, `openai/whisper-small` |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/models` | `Authorization: Key` | `generateContent` + `inline_data` | `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.5-flash-lite`, `gemini-2.0-flash` |
| `selfhosted-stt` | `http://localhost:8080/v1/audio/transcriptions` | `Authorization: Bearer` | OpenAI multipart | none predefined — supply your own |

`gemini` uses `Authorization: Key <token>`, **not** `Bearer`. `deepgram` uses `Token`, not
`Bearer`. Getting this wrong yields a 401 on the upstream side.

Any provider's base URL can be overridden per connection with
`provider_specific_data["baseUrl"]` (trailing slashes stripped). This is how `selfhosted-stt`
points at whisper.cpp, faster-whisper-server, or any OpenAI-compatible local server:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/media-providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
        "name": "local-whisper",
        "provider": "selfhosted-stt",
        "apiKey": "local",
        "baseUrl": "http://127.0.0.1:8080/v1/audio/transcriptions",
        "mediaType": "stt"
      }'
```

`selfhosted-stt` never falls back to a cloud endpoint. The API key value is not checked by
local servers — any non-empty string works.

## 3 · Transcribe — multipart (the OpenAI shape)

`multipart/form-data` is the primary path. The handler is content-type aware: a multipart
body goes down the real STT pipeline, anything else is rejected or treated as the legacy
JSON shape.

| Field | Required | Notes |
|---|---|---|
| `file` | yes | Audio bytes. The part's filename drives the MIME type when the part has no `audio/*` content type. Defaults to `audio.wav` if unnamed. |
| `model` | yes | Trimmed; empty is a 400 `Missing model`. |
| `language` | no | ISO code. Deepgram turns its absence into `detect_language=true`. |
| `prompt` | no | Biasing prompt. Also the Gemini instruction when `prompt` is absent there. |
| `response_format` | no | Passed through verbatim to OpenAI-compatible upstreams. |
| `temperature` | no | Passed through verbatim to OpenAI-compatible upstreams. |
| `deepgram_smart_format` | no | Deepgram query param. Defaults to `true`. |
| `deepgram_punctuate` | no | Deepgram query param. Defaults to `true`. |

Unknown fields are read and dropped silently — that is upstream parity, not a typo sink. If
you add a field and see no effect, check the table above.

```bash
curl -sS http://127.0.0.1:4623/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -F file=@./speech.mp3 \
  -F model="openai/whisper-1" \
  -F language=en \
  -F response_format=json
```

With a language hint and a Gemini model:

```bash
curl -sS http://127.0.0.1:4623/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -F file=@./interview.wav \
  -F model="gemini/gemini-2.5-flash" \
  -F language=en
```

Deepgram with explicit formatting:

```bash
curl -sS http://127.0.0.1:4623/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -F file=@./call.wav \
  -F model="deepgram/nova-3" \
  -F language=en \
  -F deepgram_smart_format=true \
  -F deepgram_punctuate=true
```

## 4 · Transcribe — base64 JSON (the CLI shape)

A JSON body is the backwards-compatible second shape. It uses `file_b64` instead of an
uploaded part:

```bash
curl -sS http://127.0.0.1:4623/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d "$(jq -n --arg m "openai/whisper-1" \
        --arg b "$(base64 -i ./speech.mp3)" \
        '{model:$m, file_b64:$b, file_name:"speech.mp3", language:"en"}')"
```

| Field | Required | Notes |
|---|---|---|
| `model` | yes | Same routing as multipart. |
| `file_b64` | yes | Standard base64. Invalid base64 is 400 `Invalid base64: …`. |
| `file_name` | no | Defaults to `audio.wav`; drives the MIME guess. |
| `language` / `prompt` / `response_format` / `temperature` | no | Same meaning as the multipart fields. |

Any other content type is a **400 `Content-Type must be multipart/form-data or application/json`**.
JSON bodies are capped at 200 MiB (`MAX_JSON_BODY`, stt.rs). The multipart path has
no such cap — `Multipart::from_request` inherits axum's default body limit, so a 413
there comes from that default, not from 200 MiB.

Note the `provider` key is **not** read by the handler. Routing comes from `model` alone —
see the next section.

## 5 · How `model` selects a provider

The handler calls `get_model_info()` on the `model` value. The rules, in order:

1. `combo:<name>` or a name matching a combo → **400 `Combos not supported for audio/transcriptions`**.
   Media combos can be created with `openproxy media combo create --kind stt …`, but the
   transcriptions endpoint rejects them; chain fallbacks by adding multiple credentials
   for one provider instead.
2. A leading `<provider-or-alias>/` segment wins. The split is on the **first** slash, so
   `huggingface/openai/whisper-small` resolves to provider `huggingface`, model
   `openai/whisper-small`. Provider aliases are accepted (`hf` → `huggingface`, `dg` → `deepgram`).
3. A model-alias match resolves to its target provider.
4. Otherwise the provider is inferred from the model-name prefix:

   | Prefix | Inferred provider |
   |---|---|
   | `claude-` | `anthropic` |
   | `gemini-` | `gemini` |
   | `gpt-`, `o1`, `o3`, `o4` | `openai` |
   | `deepseek-` | `openrouter` |
   | everything else | `openai` |

**The `openai` fallback is the thing that bites.** `whisper-large-v3` sent bare resolves to
`openai`, not `groq`, because nothing in the name says Groq. Always qualify:

```bash
-F model="groq/whisper-large-v3"      # correct
-F model="whisper-large-v3"           # silently goes to openai
```

Bare names are only safe for providers that match a prefix rule: `gemini/gemini-2.5-flash`
can be sent as bare `gemini-2.5-flash` and still land on gemini.

## 6 · Transcribe from the CLI

```bash
export OPENPROXY_API_KEY=...
openproxy media stt transcribe \
  --provider openai \
  --model "openai/whisper-1" \
  --file ./speech.mp3
```

- Human output prints the transcript and nothing else.
- `--robot` wraps the upstream response in the `openproxy.v1.media.stt.transcribe` envelope
  on stdout:

```bash
openproxy --robot media stt transcribe \
  --provider groq \
  --model "groq/whisper-large-v3" \
  --file ./speech.mp3 | jq -r '.data.text'
```

- The file is read from disk and base64-encoded into the JSON request, so the endpoint must
  be reachable — this is a client command, not a local-DB command. It targets `--url`
  (default `http://127.0.0.1:4623`).
- `--provider` is a required flag on this subcommand but is **not** the routing input: the
  server never reads it. Route with `--model "<provider>/<model>"`. Omitting `--model`
  sends the bare id `whisper-1`, which resolves to `openai`.

## 7 · What each provider returns

| Provider | Response body |
|---|---|
| `openai`, `groq`, `selfhosted-stt` | Upstream body and content type passed through untouched. `{"text": "..."}` by default; `response_format` can change it (`text`, `srt`, `vtt`, `verbose_json`). |
| `deepgram` | Normalized to `{"text": "..."}` from `results.channels[0].alternatives[0].transcript`. |
| `assemblyai` | Normalized to `{"text": "..."}` from the completed job. |
| `gemini` | Normalized to `{"text": "..."}` from `candidates[0].content.parts[*].text`, concatenated. |
| `huggingface` | Normalized to `{"text": "..."}` from the top-level `text` field. |

So on the six non-OpenAI paths the shape is always `{"text": …}`. Do not write a client
that assumes `verbose_json` fields exist — they only ever do for openai-compatible upstreams
that were asked for them.

AssemblyAI is asynchronous upstream: OpenProxy uploads to `/v2/upload`, submits the
transcript job with `speech_models: [model]` and `language_detection: true`, then polls
every 2s for up to 120s. A long file can hold the request open for two minutes, and
exceeding that is **504 `AssemblyAI timeout after 120s`**. With a single connection there is
no second key to fail over to.

## 8 · Account fallback

`POST /v1/audio/transcriptions` walks the provider's connections rather than giving up on
the first failure:

- Candidates are all **active** connections for the resolved provider that have a non-empty
  `apiKey` or `accessToken` and are **not currently rate-limited** (`rate_limited_until` in
  the future).
- They are tried in ascending `priority` (unset sorts last, as `999`).
- A connection is abandoned and the next one tried on upstream **401, 402, 403, 408, 429**,
  or any **5xx**.
- Other statuses — notably upstream 400 and 404 — return immediately. A bad model id does
  not burn through your other keys.
- When every candidate is exhausted, the last upstream status and message are returned. If
  the provider had no credentialed connection at all, it is **400
  `No credentials for provider: <provider>`**.

Adding two OpenAI keys under two different names and giving them priorities is the cheapest
way to get transparent retry:

```bash
openproxy media providers add --provider openai --kind stt --name openai-primary \
  --from-file - <<'JSON'
{"apiKey": "sk-primary...", "extra": {"note": "primary key"}}
JSON

openproxy media providers add --provider openai --kind stt --name openai-backup \
  --from-file - <<'JSON'
{"apiKey": "sk-backup...", "extra": {"note": "backup key"}}
JSON
```

## 9 · Verifications

```bash
# 1. Route is live and advertised
curl -sS http://127.0.0.1:4623/health
curl -sS http://127.0.0.1:4623/v1 | jq -r '.endpoints[]' | grep transcriptions

# 2. A credential exists for the STT provider
openproxy media providers list --kind stt

# 3. The catalog knows STT models
curl -sS http://127.0.0.1:4623/v1/models/stt \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq '.data | length'

# 4. End-to-end transcription. Generate a short tone locally if you have no sample:
#   macOS:  say -o ./speech.aiff "openproxy speech to text smoke test"
#   Linux:  espeak -w ./speech.wav "openproxy speech to text smoke test"
curl -sS http://127.0.0.1:4623/v1/audio/transcriptions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -F file=@./speech.wav \
  -F model="openai/whisper-1" | jq -r .text

# 5. Same request over the CLI
openproxy --robot media stt transcribe --provider openai \
  --model "openai/whisper-1" --file ./speech.wav | jq -r '.data.text'
```

## Failure modes & fixes

| Symptom | Cause | Fix |
|---|---|---|
| `400 Provider 'x' does not support STT` | `x` has no entry in `stt_config()` | Use one of the seven providers in the table above. `nvidia` and `qwen` are advertised with an `stt` service kind in the dashboard catalog but have no STT config behind them, so they land here. |
| `400 Invalid model format` | `model` was empty or whitespace-only | Always send a non-empty `model`. |
| `400 Combos not supported for audio/transcriptions` | `model` resolved to a combo | Send a concrete `<provider>/<model>`. |
| `400 Missing model` / `400 Missing required field: file` | Multipart shape incomplete | Both `file` and `model` are mandatory parts. |
| `400 Content-Type must be multipart/form-data or application/json` | Wrong content type | Set `-F` (multipart) or `-H 'content-type: application/json'`. |
| `400 No credentials for provider: x` | No active connection with a key for `x` | `openproxy media providers list --kind stt`; add one. |
| `401 on /v1/audio/transcriptions` | `requireLogin` is on (default) and the bearer is wrong or absent | Use the admin key or a key from `openproxy key add`. |
| `401 No credentials for STT provider: x` | Connection found, but its `apiKey`/`accessToken` is blank | Re-add the provider with a key. |
| `401` on the upstream but not on `/v1/*` | Wrong auth scheme | `gemini` needs `Authorization: Key`, `deepgram` needs `Authorization: Token`. Re-add the connection through `/api/media-providers` so the header is built for you. |
| Wrong provider, no error | Bare model name fell through to the `openai` prefix fallback | Qualify the model: `groq/whisper-large-v3`. |
| `504 AssemblyAI timeout after 120s` | The upstream job did not finish inside the 120s poll budget | Shorten the audio or split it; the poll is hard-capped. |
| `502 Request failed: …` | The upstream was unreachable from OpenProxy | Check egress, then the `baseUrl` override on the connection. |
| `413` / `Body too large` | Over the applicable body cap (200 MiB on the JSON path; axum's default on multipart) | Chunk the audio or compress it first. |
| `400 Invalid model ID` | A `huggingface` model id contained `..` or `//` | Use a plain repo id such as `openai/whisper-small`. |
| Local whisper server 401s | `selfhosted-stt` sent a bearer the local server rejects | Point `baseUrl` at the right path and set any non-empty `apiKey`; local servers usually ignore it. |

## When _not_ to use this skill

- Installing openproxy, wiring providers generally, or connecting a CLI tool — use
  [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md).
- Generating audio from text (`/v1/audio/speech`) — that is the TTS skill.
- Streaming transcription or websocket audio — this endpoint is request/response only.

## See also

- Entry skill: [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md)
- Route registration: `src/server/api/mod.rs` (`/v1/audio/transcriptions`, plus the
  double-prefixed `/v1/v1/audio/transcriptions` for clients that already append `/v1`)
- Handler: `src/server/api/stt.rs` — `audio_transcriptions`, `stt_config`, `resolve_stt_config`
- Kind → endpoint map: `kind_endpoint()` in `src/server/api/v1_models.rs` (`"stt" => "/v1/audio/transcriptions"`)
- CLI: `SttCmd::Transcribe` in `src/cli/media.rs` (`openproxy media stt transcribe`)
- Intentional divergences: `docs/parity-9router.md`
