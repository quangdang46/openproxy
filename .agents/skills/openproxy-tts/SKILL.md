---
name: openproxy-tts
description: Generate speech through OpenProxy's OpenAI-compatible text-to-speech API — POST /v1/audio/speech, the ?response_format=json envelope, how the model/voice string addresses each provider (OpenAI, Gemini, Fish Audio, ElevenLabs, OpenRouter, MiniMax, AWS Polly, self-hosted), combo fallback, voice listing, and the openproxy media tts CLI. Use whenever the user asks to synthesize audio, convert text to speech, pick a TTS voice, or debug a TTS provider returning the wrong container or the wrong voice.
---

# openproxy-tts — text to speech

OpenProxy exposes an OpenAI-compatible TTS surface on `127.0.0.1:4623` and routes each request to whichever TTS provider you name in the `model` field. This skill covers:

1. Registering a TTS provider
2. Synthesizing audio over HTTP, and reading the response
3. Addressing model + voice (they share one field)
4. Provider-specific parity notes (Gemini, Fish Audio, OpenAI, OpenRouter, ElevenLabs, MiniMax, AWS Polly, self-hosted)
5. Combo fallback and account rotation
6. The `openproxy media tts` CLI

Every step is non-interactive and safe to run unattended. If the server is not up yet, follow `.agents/skills/openproxy/SKILL.md` first.

## 0 · Preconditions

```bash
curl -sS http://127.0.0.1:4623/health

# A proxy API key. Any key works, including the admin key from `server init`.
export OPENPROXY_API_KEY="$(openproxy key add tts-agent --auto --robot | jq -r '.data.key')"

# Which TTS providers have credentials right now
openproxy media providers list --kind tts
```

`/v1/*` is gated by the `require_api_key` setting (falling back to `require_login`). When it is on, every call needs `Authorization: Bearer $OPENPROXY_API_KEY`.

**The `openproxy media …` commands need that key exported too.** They talk to the running server rather than reading `db.json` directly, so an unset `OPENPROXY_API_KEY` surfaces as a confusing `server returned 401 Unauthorized: {"code":"invalid_api_key","message":"Invalid API key"}` — the key is usually simply *missing*, not wrong. Export it in the same shell as the curl calls, and use `--api-key` (or `--url`) when talking to a non-default server.

## 1 · Register a TTS provider

TTS credentials live in the media-provider registry, keyed by a provider id. The id is what you put in front of the `model` field.

```bash
cat > /tmp/tts.json <<'JSON'
{ "apiKey": "sk-...", "isActive": true }
JSON

openproxy media providers add \
  --provider openai --kind tts --name "OpenAI TTS" \
  --from-file /tmp/tts.json
```

`--from-file` merges on top of `{provider, kind, name}`, so it only carries the extra fields. `-` reads stdin. Edit with `openproxy media providers edit <id> --from-file -`; remove with `openproxy media providers delete <id> --kind tts`.

A provider with no active connection fails fast with `400 No credentials for provider: <id>` — verify with `openproxy media providers list --kind tts` before debugging anything else.

### TTS provider ids

Dedicated adapters (own request shape):

| Id | Notes |
|---|---|
| `openai` | `model/voice`, defaults to `gpt-4o-mini-tts` + `alloy` |
| `gemini` | 30 prebuilt voices, default `Kore`; returns WAV |
| `openrouter` | `provider/model/voice`; SSE chat completions |
| `elevenlabs` | `model/voice`, defaults to `eleven_flash_v2_5` |
| `minimax`, `minimax-cn` | `speech-2.8-hd` + `English_expressive_narrator` by default |
| `google-tts` | `x-goog-api-key`, `text:synthesize` |
| `edge-tts` | unauthenticated scrape, browser User-Agent |
| `aws-polly`, `polly` | SigV4-signed; needs `accessKeyId` / `secretAccessKey` / `region` |
| `xiaomi-mimo` | voice rides the upstream `audio.voice` field |
| `local-device` | host OS speech (macOS / Windows) |
| `selfhosted-tts` | any local server exposing `/v1/audio/speech` |

Config-driven generic formats (the `ttsConfig.format` path, with a per-provider default URL):

| Id | Default upstream |
|---|---|
| `fish-audio`, `fish` | `https://api.fish.audio/v1/tts` |
| `hyperbolic` | `https://api.hyperbolic.xyz/v1/audio/generation` |
| `deepgram` | `https://api.deepgram.com/v1/speak` |
| `nvidia` | `https://integrate.api.nvidia.com/v1/audio/synthesis` |
| `huggingface` | `https://api-inference.huggingface.co/models` |
| `inworld` | `https://api.inworld.ai/tts/v1/voice` |
| `cartesia` | `https://api.cartesia.ai/tts/bytes` |
| `playht` | `https://api.play.ht/api/v2/tts/stream` |
| `coqui` | `http://localhost:5002/api/tts` |
| `tortoise` | `http://localhost:5000/api/tts` |
| `selfhosted-tts` | `http://localhost:8880/v1/audio/speech` |

Override any default per connection with a `baseUrl` field — it wins over the built-in default:

```bash
echo '{"apiKey":"…","isActive":true,"baseUrl":"https://my-tts.internal"}' \
  | openproxy media providers add --provider selfhosted-tts --kind tts --name "Local Kokoro" --from-file -
```

## 2 · Synthesize speech

The endpoint is `POST /v1/audio/speech`. (`/v1/v1/audio/speech` is registered as an alias for clients that double the prefix — use the plain one.)

```bash
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o-mini-tts/alloy","input":"Ship it on Friday."}' \
  -o speech.mp3

file speech.mp3
```

### How audio comes back: binary by default, JSON on request

The adapter always produces an internal `{audio: "<base64>", format: "mp3"}` object. What the client sees is decided by the **`?response_format` query parameter**, read in `src/server/api/media.rs`:

| Request | Response |
|---|---|
| no `?response_format` | **raw audio bytes**, `Content-Type: audio/<format>` |
| `?response_format=json` | the `{audio, format}` JSON envelope, base64 string |
| any other value | raw audio bytes |

```bash
# Binary (default) — save straight to a file
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"gemini/gemini-3.1-flash-tts-preview/Kore","input":"Hello."}' \
  -o hello.wav

# JSON envelope — for agents that want metadata alongside the payload
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech?response_format=json' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o-mini-tts/alloy","input":"Hello."}' | jq -r '.format, (.audio|length)'
```

Three rules worth internalizing:

1. **`response_format` is a query parameter, not a body field.** Sending `"response_format":"json"` in the JSON body does not switch the response shape. (The OpenAI-shaped body field is accepted by some upstreams, but OpenProxy's binary-vs-JSON decision reads only the query string.)
2. **The container is whatever the provider returned** — `audio/mp3`, `audio/wav`, `audio/ogg`. Gemini and OpenRouter always yield `wav`; OpenAI, ElevenLabs, Fish Audio and Polly default to `mp3`. Do not assume the extension you asked for.
3. **Errors are always JSON.** The binary transform only runs on a 2xx, so a `400`/`429`/`502` comes back as `{"error":{"type":…,"code":…,"message":…}}` no matter which `response_format` you passed.

## 3 · Addressing model and voice

**The voice rides on the `model` field as `model/voice`.** There is no top-level `voice` field in the request contract — the TTS dispatcher reads only `model`, `input`, and the optional `language` and `style` strings.

```jsonc
{"model": "<provider>/<model>/<voice>", "input": "text to speak"}
```

- `openai/gpt-4o-mini-tts/alloy` → provider `openai`, model `gpt-4o-mini-tts`, voice `alloy`
- `fish-audio/s2.1-pro-free/my-reference-id` → provider `fish-audio`, model in the `model` header, voice as `reference_id`
- `gemini/Kore` → a bare voice; Gemini fills in its default model
- `aws-polly/neural/Salli.ogg_opus` → engine / voice / output extension

A `combo:<name>` prefix (or a bare combo name) routes through a media combo instead — see §5.

## 4 · Provider parity notes

### Gemini — PCM wrapped in WAV

Gemini is not an OpenAI-shaped speech API. OpenProxy calls `generateContent` with `responseModalities: ["AUDIO"]` and a `prebuiltVoiceConfig`, gets back raw **PCM L16**, and wraps it in a WAV header itself: 24 kHz, mono, 16-bit. So a Gemini TTS call always comes back as `audio/wav` regardless of what you requested.

- Default model `gemini-3.1-flash-tts-preview`; also recognized: `gemini-2.5-flash-preview-tts`, `gemini-2.5-pro-preview-tts`
- 30 prebuilt voices, default `Kore` — `Zephyr Puck Charon Kore Fenrir Leda Orus Aoede Callirrhoe Autonoe Enceladus Iapetus Umbriel Algieba Despina Erinome Algenib Rasalgethi Laomedeia Achernar Alnilam Schedar Gacrux Pulcherrima Achird Zubenelgenubi Vindemiatrix Sadachbia Sadaltager Sulafat`
- `"language"` is honored by rewriting the prompt: text without `": "` becomes `Say in {language}: {text}`, otherwise `Say: {text}`. Include a `": "` yourself to pass the text through untouched.

```bash
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"gemini/gemini-3.1-flash-tts-preview/Puck","input":"Deployment is green.","language":"en-GB"}' \
  -o green.wav
```

If Gemini returns no audio, the error names the cause: `Gemini TTS returned no audio (finishReason: …, voice: …, model: …)`.

### Fish Audio — model in a header, voice as `reference_id`

Fish Audio is the one provider that does not take the model in the JSON body. OpenProxy posts to `https://api.fish.audio/v1/tts` with the model id in an HTTP **`model` header**, `{"text": …, "format": "mp3"}` in the body, and your voice as **`reference_id`** (a preset or a cloned-voice model id). When no model is given it falls back to `s2.1-pro-free`. The result is mp3.

```bash
# model → `model` header, voice → reference_id
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"fish-audio/s2.1-pro-free/your-reference-id","input":"Ship it on Friday."}' \
  -o ship.mp3
```

Both `fish-audio` and the short alias `fish` resolve to this format.

### OpenAI

`model/voice` splits into the two upstream fields. With no slash the whole string is the **voice** and the model defaults to `gpt-4o-mini-tts`; empty falls back to `gpt-4o-mini-tts` + `alloy`. The upstream body is rebuilt as `{model, voice, input}` only — your `response_format` is not forwarded to OpenAI, so the container is always `mp3`. `baseUrl` in `provider_specific_data` overrides `https://api.openai.com` (use it for OpenAI-compatible gateways).

### OpenRouter — three segments, chat completions under the hood

`provider/model/voice` is the full form; `provider/model` keeps the whole string as the model and takes the last segment as the voice; a bare string is a voice against the default `openai/gpt-4o-mini-tts`. OpenProxy POSTs a streaming chat completion with `modalities: ["text","audio"]` and `audio: {voice, format: "wav"}`, concatenates every `choices[0].delta.audio.data` chunk from the SSE, and returns WAV. It sends the `HTTP-Referer` and `X-Title` headers OpenRouter expects.

### ElevenLabs

`model/voice`, or a bare voice id (which pairs with the default `eleven_flash_v2_5`). Auth is the `xi-api-key` header. Response is mp3; bodies under 1 KB are rejected as empty audio.

### MiniMax

Longest-known `speech-*` prefix wins, so `speech-2.8-turbo/warrior` resolves model and voice correctly. Defaults: `speech-2.8-hd` + `English_expressive_narrator`. `minimax` uses `https://api.minimax.io/v1/t2a_v2`, `minimax-cn` uses `https://api.minimaxi.com/v1/t2a_v2`.

### AWS Polly

`model` is `[engine/]voice[.ext]` — the extension is one of `mp3`, `pcm`, `ogg`, `wav`, `ogg_vorbis`, `ogg_opus`, and it sets the output format. Defaults: engine `neural`, voice `Joanna`, format `mp3`, region `us-east-1`. Text starting with `<speak` is sent as `TextType: ssml` automatically. Credentials go in `provider_specific_data` as `accessKeyId`, `secretAccessKey`, `region` — a plain `apiKey` is not enough, because the request is SigV4-signed against `polly.{region}.amazonaws.com`.

### Self-hosted

`selfhosted-tts` treats `baseUrl` as the **server root** and appends `/v1/audio/speech`. A baseUrl that already ends in that path is accepted as-is (trailing slashes trimmed), so both `http://localhost:8880` and `http://localhost:8880/v1/audio/speech/` work. It never falls back to a cloud endpoint. The upstream request is OpenAI-shaped (`model`, `voice`, `input`, `response_format: mp3`).

### Google TTS / Edge TTS

`google-tts` hits `…/text:synthesize?key=` with an `x-goog-api-key` header; the `model` field is the language code. `edge-tts` uses its baseUrl as-is (no `/audio/speech` suffix) and sends a browser User-Agent, because those endpoints reject anything that smells like curl.

## 5 · Combo fallback and account rotation

A **media combo** chains several providers and is the TTS equivalent of a chat combo. TTS and image generation expand combos; STT, embeddings and video do not.

```bash
openproxy media combo create --kind tts --name tts-chain --members openai,gemini
openproxy media combo list
```

```bash
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"combo:tts-chain","input":"Fallback test."}' -o out.mp3
```

Each member is tried in configured order and the first 2xx wins. If every member fails, the last member's own error is returned — a real upstream message, not a synthesized combo error.

Within one provider, connections are selected by ascending `priority`, and a `401`, `403` or `429` rotates to the next connection automatically. Other statuses are returned as-is without rotation.

## 6 · Discover voices

```bash
openproxy media tts voices
openproxy media tts voices --provider elevenlabs --lang en

# Same data over HTTP
curl -sS -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  'http://127.0.0.1:4623/v1/audio/voices?provider=deepgram&lang=en'
```

`GET /v1/audio/voices` accepts only `elevenlabs`, `deepgram`, `inworld`, `minimax`, `minimax-cn`, `edge-tts`, and `local-device`; anything else is a `400` naming that list. Gemini's 30 prebuilt voices are **not** served here — use the list in §4. The underlying per-provider endpoints are `/api/media-providers/tts/voices?provider=…` and `/api/media-providers/tts/{elevenlabs,deepgram,inworld,minimax}/voices`.

To see which TTS models OpenProxy knows about:

```bash
curl -sS -H "Authorization: Bearer $OPENPROXY_API_KEY" http://127.0.0.1:4623/v1/models/tts

# One model card. `id` is required (`model` is an accepted alias) and the
# voice segment is NOT part of the lookup key.
curl -sS -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  'http://127.0.0.1:4623/v1/models/info?id=openai/gpt-4o-mini-tts'
# → {"endpoint":"/v1/audio/speech","id":"openai/gpt-4o-mini-tts","kind":"tts",
#    "model":"gpt-4o-mini-tts","provider":"openai","routeKind":"Direct", …}
```

`/v1/models/info` fills in the `endpoint` field from the canonical kind→endpoint map; for kind `tts` it is always `/v1/audio/speech`. Querying with a `model/voice` string returns `404 Model not found` — look the model up without the voice.

## 7 · CLI

`openproxy media tts speak` is the only command in the tree that writes raw bytes to stdout.

```bash
# Binary to a file — do NOT add --robot here, see the note below
openproxy media tts speak \
  --provider openai --model 'gpt-4o-mini-tts/alloy' \
  --voice alloy --text 'Ship it on Friday.' > ship.mp3

# Read from stdin
printf 'Deploy is green.\n' \
  | openproxy media tts speak --provider gemini --model Kore --text - > green.wav

openproxy media tts speak --help
```

Flags: `--provider` (required), `--voice` (required), `--model` (default `""`), `--text` (default `-`), `--format` (default `mp3`).

Two things to know about the CLI:

- **The voice belongs in `--model`, as `model/voice`.** The `--voice` flag populates a `voice` field in the outgoing body, and the TTS dispatcher does not read it. Passing only `--voice` without a `--model` sends the provider id as the model string, which the adapter then reads as a bare voice — usually an upstream error. Use `--model 'gpt-4o-mini-tts/alloy'` (or `--model Kore` for Gemini) and treat `--voice` as required-but-inert.
- **`--robot` and stdout do not mix for file output.** In robot mode the CLI first prints the `openproxy.v1.media.tts.speak` envelope (`{"bytes": N, "content_type": …}`) and *then* the raw audio on the same stream. Redirecting to a file gives you JSON prepended to your audio. Split the two channels (`--robot | head -c …` for metadata), or drop `--robot` and redirect.

Other media commands that matter for TTS:

```bash
openproxy media providers list --kind tts
openproxy media providers add   --provider <id> --kind tts --name <name> --from-file -
openproxy media providers edit  <connection-id> --from-file -
openproxy media providers delete <connection-id> --kind tts
openproxy media combo list
openproxy media combo create --kind tts --name <name> --members <a,b,c>
```

## Verification

```bash
# 1. Provider credentials are registered
openproxy media providers list --kind tts

# 2. Binary path returns bytes, not JSON
curl -sS -o /tmp/tts.mp3 -w '%{content_type} %{size_download}\n' \
  -X POST 'http://127.0.0.1:4623/v1/audio/speech' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o-mini-tts/alloy","input":"Verification ping."}'
file /tmp/tts.mp3          # → Audio file, MP3 / WAVE, not "JSON text"

# 3. JSON path returns the base64 envelope
curl -sS -X POST 'http://127.0.0.1:4623/v1/audio/speech?response_format=json' \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o-mini-tts/alloy","input":"Verification ping."}' \
  | jq -e '.audio and .format' >/dev/null && echo "envelope ok"
```

## Common failure modes & fixes

| Symptom | Fix |
|---|---|
| `400 No credentials for provider: <id>` | No active connection for that provider id. `openproxy media providers add --kind tts …`, or check `--kind tts` in the list output. |
| `400 Missing model` | The body has no non-empty `model`. Include the provider prefix: `openai/gpt-4o-mini-tts/alloy`. |
| `400 Invalid model format` | The `model` string had no provider prefix and matched no combo/alias. Use `provider/model/voice`. |
| Combo name 400s with `Combo not found: <name>` or `Combo has no models` | The combo is missing or empty. `openproxy media combo list`, or create it with `--kind tts`. |
| Audio bytes, but the file will not play | You saved a JSON envelope (or a `--robot` metadata line) instead of audio. Drop `--robot`; re-save with `-o file` and check `file`. |
| Voice ignored — wrong speaker | You put the voice in a `voice` field. It must be the last segment of `model`: `openai/gpt-4o-mini-tts/nova`. |
| `Combos not supported` on a TTS combo | The combo was created under the wrong kind. `openproxy media combo create --kind tts …`. |
| `provider must be one of: elevenlabs, deepgram, …` | `GET /v1/audio/voices` only serves that subset. For Gemini use the 30 prebuilt voice names. |
| `Gemini TTS returned no audio (finishReason: …)` | The voice name is not one of the 30 prebuilt voices, or the model id is unknown. |
| `Upstream returned empty audio` | The provider answered with under 100 bytes — usually a bad key, and the adapter refuses to pass it on. |
| `401`/`403` then success | Expected: OpenProxy rotated to the next connection. Check `openproxy --robot doctor` if every request rotates. |
| `429` persists across calls | The account is rate-limited and marked unavailable. Wait for `rate_limited_until` or add a second connection. |

## When _not_ to use this skill

- Transcription / speech-to-text → `/v1/audio/transcriptions` (a separate endpoint and skill).
- Music generation → `POST /v1/audio/music`.
- Video with audio → `/v1/videos/generations`.
- Server install, provider config, or CLI-tool wiring in general → `.agents/skills/openproxy/SKILL.md`.
- Combo dispatch debugging across every kind → `.agents/skills/openproxy-combos/SKILL.md`.

## See also

- Entry skill: `.agents/skills/openproxy/SKILL.md`
- TTS dispatch registry: `src/core/media/tts/mod.rs` (provider → adapter / format map)
- Per-provider adapters: `src/core/media/tts/{openai,gemini,openrouter,elevenlabs,minimax,aws_polly,generic_formats}.rs`
- HTTP layer: `src/server/api/media.rs` (`audio_speech`, `tts_binary_or_json`, `combo_expands`)
- Intentional divergences: `docs/parity-9router.md`
