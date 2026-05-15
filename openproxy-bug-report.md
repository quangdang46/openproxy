# OpenProxy — Báo cáo bug & test functional

- **Repo**: https://github.com/quangdang46/openproxy
- **Cách cài**: `curl -fsSL "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh" | bash`
- **Phiên bản cài đặt**: GitHub Release **v0.1.0** (Linux x86_64) – binary tự báo dashboard **v0.4.16** (xem Bug 11)
- **Môi trường test**: Ubuntu Linux, `/home/ubuntu/.local/bin/openproxy`, `~/.openproxy/`
- **Tổng quan**: Install script chạy OK, server start được, một số CLI flow OK. Nhưng **dashboard hoàn toàn không sử dụng được qua URL mặc định** (vòng lặp redirect), `--version` không tồn tại, default bind là `0.0.0.0` (mâu thuẫn README & rủi ro bảo mật), và rất nhiều cú pháp lệnh trong README / SKILL.md không khớp với CLI thật.

---

## TL;DR — Mức độ nghiêm trọng

| # | Bug | Mức độ |
|---|---|---|
| 1 | Dashboard web: vòng lặp redirect `/` → `/dashboard` → `/dashboard` (stub redirect). Toàn bộ UI không truy cập được qua URL mặc định | **Blocker** |
| 2 | `openproxy --version` / `-V` / `version` đều không hoạt động (trong khi README và SKILL.md đều dùng) | **Cao** |
| 3 | `--host` mặc định = `0.0.0.0` (binding LAN) trong khi README ghi default là `127.0.0.1` → rò rỉ dashboard ra LAN, kèm `INITIAL_PASSWORD=123456` + `REQUIRE_API_KEY=false` mặc định | **Cao (bảo mật)** |
| 4 | Sau khi install, không có admin API key được tạo; `server init` báo conflict vì `db.json` đã tồn tại → người dùng không có cách "officially" lấy key như SKILL.md mô tả mà không `--force` (sẽ xoá DB) | **Cao** |
| 5 | `openproxy doctor` lần đầu báo `data_dir` và `db_file` FAIL nhưng `db_loadable` OK, đồng thời exit code = 0 dù có FAIL (lần kế tiếp lại exit 1) → check không nhất quán | **Trung bình** |
| 6 | `openproxy server start --no-open` (theo SKILL.md & README) không hợp lệ — `--no-open` là global flag, không phải subcommand flag | **Trung bình** |
| 7 | `openproxy key add <NAME>` (theo README) thiếu argument; thực tế CLI bắt buộc `key add <NAME> <KEY>` | **Trung bình** |
| 8 | `openproxy provider add <NAME> <CONFIG>`: tên truyền vào bị gán vào field `provider` (không phải `name`); response trả về `name: null` và `authType: oauth` dù truyền `apiKey` — mismatch với schema example | **Trung bình** |
| 9 | `openproxy combo create <name>` (theo README) sai cú pháp — thực tế là `combo create --name <NAME>` (flag, không positional) | **Thấp** |
| 10 | `openproxy quota` và `openproxy usage` (theo README) không in gì — thực tế phải gọi subcommand (`quota list`, `usage summary`, ...). Help vẫn exit 0 nên dễ tưởng nhầm là "OK chưa có gì" | **Thấp** |
| 11 | `settings version` trả về vô lý: `currentVersion: 0.4.16`, `latestVersion: 0.0.2`, `hasUpdate: false` (latest < current?). Đồng thời release tag là `v0.1.0` → 3 con số version mâu thuẫn nhau | **Trung bình** |
| 12 | `openproxy chat send --prompt "<text>"` báo `read input file '<text>': No such file or directory`. Help ghi "Prompt string, or `-` to read from stdin" — thực tế code dùng `read_input()` luôn coi giá trị ≠ `-` là **path file** | **Cao** (sai documented behaviour) |
| 13 | `key add` qua CLI khi server đang chạy: key được lưu vào `db.json` nhưng phải có file-watcher reload (delay ~vài giây); nếu gọi `/v1/models` ngay sau `key add` thì 401 "Invalid API key" → race condition rõ rệt | **Trung bình** |
| 14 | Validation message kém: `provider validate --provider openai --api-key sk-fake` → `valid: false, error: null` (không có lý do); `translator translate --from openai --to claude` → `"provider and model required"` mặc dù help nói `--model` mặc định `""` | **Thấp** |
| 15 | `web/scripts/build-npm-package.mjs` được tham chiếu trong `package.json` (scripts `package:npm`, `dist:npm`, `verify:npm`) nhưng file **không tồn tại** trong repo → 3 npm script này gãy | **Trung bình** |
| 16 | `pnpm run build` không được chạy trong release pipeline / `build.rs` kiểm tra `web/dist/index.html` nhưng release v0.1.0 lại embed file `index.html` chỉ chứa meta-refresh stub (147 byte) → root cause của Bug 1 | **Blocker** |

---

## Chi tiết từng bug

### Bug 1 — Dashboard web: vòng lặp redirect, UI không truy cập được (BLOCKER)

**Repro**:
```bash
openproxy server start --detach
curl -i http://127.0.0.1:4623/
```

`/` trả về:
```
HTTP/1.1 200 OK
content-type: text/html
content-length: 147

<!DOCTYPE html><script></script> <meta http-equiv="refresh" content="0;url=/dashboard"><p>Redirecting to <a href="/dashboard">/dashboard</a>...</p>
```

Và `/dashboard` (cùng tất cả sub-route `/dashboard/endpoint`, `/dashboard/providers`, `/dashboard/combos`, `/dashboard/usage`, `/dashboard/quota`, `/dashboard/mitm`, `/dashboard/cli-tools`, `/dashboard/profile`, `/dashboard/console-log`) đều trả về cùng nội dung 147 byte đó → vòng lặp meta-refresh vô tận.

Trên browser:

![Browser stuck at "Redirecting to /dashboard..."](https://app.devin.ai/attachments/f0427f87-cd1d-42ce-bacd-3bbc627df299/screenshot_486611c131f54e71b44df0dcae6a38ea.png)

**Root cause**:
- `web/astro.config.mjs` set `build.format: 'file'` → Astro sinh ra `dashboard.html`, `dashboard/endpoint.html`, ... (có đuôi `.html`).
- `src/pages/index.astro` (frontmatter rỗng) render ra body chỉ là `<meta http-equiv="refresh" content="0;url=/dashboard">` → file 147 byte chính là `index.html` đã build.
- `src/server/dashboard/mod.rs::serve_embedded()` khi nhận `/dashboard`:
  1. `normalize_asset_path` → `"dashboard"` (không có đuôi)
  2. `lookup_embedded("dashboard")` → không thấy (file thật là `dashboard.html`)
  3. `looks_like_asset("dashboard")` → false (không có dấu `.`)
  4. Fallback `lookup_embedded("index.html")` → trả về **chính cái stub redirect** → loop
- Khi gọi trực tiếp `curl http://127.0.0.1:4623/dashboard.html` thì SPA load OK (5829 byte, render đầy đủ menu, dark-mode, etc. — xác nhận asset có embed). Nhưng tất cả nav link trong SPA cũng dùng path không `.html` (`href="/dashboard/endpoint"`, ...) nên click cũng dẫn về stub.

**Fix gợi ý**: Trong `serve_embedded()`, trước khi fallback về `index.html`, hãy thử `lookup_embedded(format!("{path}.html"))`. Hoặc đổi Astro sang `build.format: 'directory'` để sinh `dashboard/index.html`.

**Liên quan**: `src/server/dashboard/mod.rs:80-98`, `web/astro.config.mjs:21-25`, `web/src/pages/index.astro`.

---

### Bug 2 — `openproxy --version` không hoạt động (CAO)

**Repro**:
```bash
$ openproxy --version
error: unexpected argument '--version' found

  tip: a similar argument exists: '--verbose'

$ openproxy -V
error: unexpected argument '-V' found

$ openproxy version
error: unrecognized subcommand 'version'
```

README §"Verify" và `.agents/skills/openproxy/SKILL.md` §1 đều bảo agent chạy `openproxy --version` để xác minh cài đặt → bị chặn ngay bước này. Một AI agent đi theo SKILL.md sẽ stuck. `openproxy settings version` (subcommand) có hoạt động nhưng cần server đang chạy → không phải lệnh dùng để smoke-test sau install.

**Fix gợi ý**: thêm `#[clap(version)]` vào root `Cli` struct.

---

### Bug 3 — Default bind `0.0.0.0`, mâu thuẫn README và rủi ro bảo mật (CAO)

**Repro**:
```bash
$ openproxy --help | grep -A1 '\-\-host'
      --host <HOST>
          [env: HOST=] [default: 0.0.0.0]

$ openproxy server start --detach
Started openproxy (pid 6670) on 0.0.0.0:4623
```

README §Configuration ghi:
> `HOSTNAME` | `127.0.0.1` | Bind host. Set `0.0.0.0` to expose on LAN.

Mismatch:
1. Tên env: README dùng `HOSTNAME`, CLI thực tế đọc env `HOST`.
2. Default: README nói `127.0.0.1`, CLI thực tế `0.0.0.0`.

Mặc định `0.0.0.0` + `REQUIRE_API_KEY=false` + `INITIAL_PASSWORD=123456` ⇒ bất kỳ ai trong LAN đều có thể:
- Mở `http://<host-ip>:4623/dashboard.html` (xem Bug 1, vẫn truy cập được nếu biết đường dẫn).
- POST `/api/auth/login` với `{"password":"123456"}` và lấy được session cookie quản trị.
- Gọi `/v1/*` không cần Authorization (vì `REQUIRE_API_KEY=false`).

Đây là rủi ro thực tế. Nên đổi default `--host` về `127.0.0.1` cho khớp README.

---

### Bug 4 — Sau install, không có admin API key; `server init` block luôn (CAO)

`.agents/skills/openproxy/SKILL.md` §2 nói `server init` "emits **one** fresh admin API key — shown exactly once" và chỉ dẫn agent capture nó. Thực tế:

```bash
$ openproxy --robot server init
{"schema":"openproxy.v1.error","ok":false,
 "error":{"code":"conflict","message":"db.json already exists at /home/ubuntu/.openproxy/db.json (use --force to overwrite)"},"meta":{}}
```

`db.json` được tạo tự động bởi lần gọi `openproxy doctor` đầu tiên (xem Bug 5) — chứa apiKeys rỗng. SKILL.md cảnh báo "**Do not force without asking the user**" → agent đi đúng skill sẽ rơi vào deadlock: không có key, không được force.

Fallback duy nhất là login bằng password mặc định `123456` qua `/api/auth/login` rồi mint key qua dashboard. Nhưng dashboard lại không truy cập được (Bug 1).

Đề xuất: `server init` nên tự coi data dir trống (apiKeys=[]) là idempotent và mint admin key thay vì lỗi conflict. Hoặc lần đầu start server tự sinh admin key và log ra stdout/file một lần.

---

### Bug 5 — `openproxy doctor` không nhất quán & exit code sai (TRUNG BÌNH)

Lần đầu (data dir chưa tồn tại):
```
$ openproxy doctor
openproxy doctor:
  [FAIL] data_dir — /home/ubuntu/.openproxy does not exist (will be created on first write)
  [FAIL] db_file  — /home/ubuntu/.openproxy/db.json not found (run 'openproxy server start' once to initialize)
  [ok  ] db_loadable — 0 providers, 0 keys, 0 pools, 0 combos
  [FAIL] server_reachable — http://127.0.0.1:4623/health unreachable
result: at least one check failed
$ echo $?
0       # ← sai, có FAIL mà vẫn exit 0
```

Sau khi gọi `doctor` lần nữa:
```
$ ls -la ~/.openproxy/
-rw-r--r-- … db.json
-rw-r--r-- … usage.json
```

Việc chạy `doctor` đã tự tạo `db.json` (do `db_loadable` check có side-effect lazy-init). Hai check trước (`data_dir`, `db_file`) chạy trước khi `db_loadable` lazy-init, nên báo FAIL mặc dù sau đó file đã tồn tại. Lần kế tiếp `doctor` chạy thì exit code thành `1`.

Đề xuất:
- `data_dir`/`db_file` check là read-only.
- `db_loadable` check không nên có side-effect tạo file. Hoặc nếu có thì phải report đúng.
- Exit code: bất kỳ FAIL nào → non-zero ngay lần đầu.

---

### Bug 6 — `openproxy server start --no-open` không hợp lệ (TRUNG BÌNH)

`.agents/skills/openproxy/SKILL.md` §3 và README đều khuyên agent:
```bash
openproxy server start --detach --no-open
```

Thực tế:
```
$ openproxy server start --detach --no-open
error: unexpected argument '--no-open' found

Usage: openproxy server start --detach
```

`--no-open` là **global** flag (xuất hiện trước subcommand), nên cú pháp đúng là:
```bash
openproxy --no-open server start --detach
```

→ Cần sửa README và SKILL.md hoặc cho `server start` cũng nhận flag này. Một agent dùng SKILL.md word-for-word sẽ bị crash ở bước 3.

---

### Bug 7 — `openproxy key add <name>` (theo README) thiếu argument (TRUNG BÌNH)

README §CLI reference:
```
openproxy key add <name>
```

Thực tế:
```
$ openproxy key add admin-test
error: the following required arguments were not provided:
  <KEY>

Usage: openproxy key add <NAME> <KEY>
```

CLI bắt buộc cả **secret value** truyền tay. Không có flag `--generate` hay tương đương để CLI tự sinh secret.

Đề xuất: thêm `--auto` / `--generate` để sinh secret ngẫu nhiên và in ra một lần.

---

### Bug 8 — `provider add` gán nhầm field, default `authType=oauth` (TRUNG BÌNH)

`schema example provider`:
```json
{"apiKey":"sk-...","isActive":true,"name":"openai-main","priority":10,"provider":"openai"}
```

Thử `provider add` với CLI và NAME `openai-test`, payload đầy đủ:
```bash
openproxy --robot provider add openai-test \
  '{"provider":"openai","apiKey":"sk-test-123","isActive":true,"priority":10}'
```

Response:
```json
{
  "id": "07e7d47e-...",
  "name": null,                  ← argument NAME bị bỏ
  "provider": "openai-test",     ← argument NAME bị nhét vào field `provider`
  "authType": "oauth",           ← dù gửi apiKey, vẫn default sang oauth
  "apiKey": "sk-test-123",
  ...
}
```

Có 2 vấn đề:
1. CLI dường như override field `provider` trong payload bằng `<NAME>` positional → schema example và CLI semantics không khớp.
2. Mặc dù body có `apiKey`, `authType` vẫn `oauth` thay vì `api_key` → CLI không suy luận auth type từ payload.

Sau bug này, `/v1/models` trả về **rỗng** (`{"object":"list","data":[]}`) thay vì danh sách 66 model built-in như khi chưa có provider. → Việc thêm 1 provider rác cũng làm hỏng models list. Có thể là behaviour mong muốn (chỉ show models của configured providers), nhưng kết hợp với bug 8 thì người dùng dễ hoang mang.

---

### Bug 9 — `combo create <name>` (README) sai cú pháp (THẤP)

README:
```bash
openproxy combo create <name> --models cc/opus,glm/glm-5
```

Thực tế:
```
$ openproxy combo create my-stack --models openai/gpt-4o
error: the following required arguments were not provided:
  --name <NAME>
```

Cú pháp đúng:
```bash
openproxy combo create --name my-stack --models openai/gpt-4o,anthropic/claude-3-5-sonnet
```

→ README cần cập nhật.

---

### Bug 10 — `openproxy quota` / `openproxy usage` không in gì (THẤP)

README:
```
openproxy quota
openproxy usage
```

Thực tế cả hai chỉ in help text & exit 0, không có default subcommand. Một AI agent sẽ tưởng `quota=0 / usage=0`.

Đề xuất: gọi `quota list` / `usage summary` mặc định, hoặc sửa README.

---

### Bug 11 — Version mâu thuẫn (TRUNG BÌNH)

```
$ openproxy --robot settings version
{"data":{"currentVersion":"0.4.16","hasUpdate":false,"latestVersion":"0.0.2"},...}
```

- GitHub Release tag: `v0.1.0`
- `web/package.json` version: `0.4.16` ← cái này được report là "currentVersion"
- `latestVersion`: `0.0.2` (?!)
- `hasUpdate: false` dù `currentVersion > latestVersion`

3 con số version đến từ 3 nơi khác nhau và không có dấu hiệu được đồng bộ. Không có cách nào hỏi binary "anh là binary phiên bản nào của crate `openproxy`" — `Cargo.toml` version, dashboard version và GitHub release version không thống nhất.

---

### Bug 12 — `chat send --prompt "<text>"` coi text là **đường dẫn file** (CAO)

```
$ openproxy chat send --model my-stack --prompt "hello"
Error: read input file 'hello'

Caused by:
    No such file or directory (os error 2)
```

Trong khi `chat send --help` nói:
```
--prompt <PROMPT>   Prompt string, or `-` to read from stdin [default: -]
```

Source `src/cli/runtime.rs:548-554` (`read_input`):
```rust
pub fn read_input(from: &str) -> anyhow::Result<String> {
    if from == "-" {
        read_stdin_to_string()
    } else {
        std::fs::read_to_string(from).with_context(|| format!("read input file '{from}'"))
    }
}
```

→ Code **luôn** coi `--prompt` value là path. Cách duy nhất gửi prompt inline là pipe stdin:
```bash
echo "hello" | openproxy chat send --model my-stack --prompt -
```

Sửa nhanh: thêm flag `--prompt-text` chỉ định string, hoặc auto-detect file tồn tại trước khi `read_to_string`.

---

### Bug 13 — Hot-reload `key add` race (TRUNG BÌNH)

```bash
openproxy --robot key add k1 secret123
curl http://127.0.0.1:4623/v1/models -H "Authorization: Bearer secret123"
# → 401 "Invalid API key"   ← ngay lập tức

# Đợi ~5 giây để db.watcher reload:
sleep 5
curl http://127.0.0.1:4623/v1/models -H "Authorization: Bearer secret123"
# → 200
```

Server log:
```
INFO openproxy::db::watcher: db file watcher active path=/home/ubuntu/.openproxy
INFO openproxy::db::watcher: db.json reloaded into in-memory snapshot   ← delay
```

→ Race giữa CLI write và watcher reload. Người dùng làm theo doc (gọi `key add` rồi gọi `/v1/*`) có thể fail. Đề xuất:
- CLI `key add` gọi luôn `/api/keys` (qua HTTP) thay vì viết trực tiếp `db.json` khi server đang chạy.
- Hoặc CLI block đến khi nhận được tín hiệu reload xong.

---

### Bug 14 — Validation/Translate error message kém (THẤP)

`provider validate`:
```bash
$ openproxy --robot provider validate --provider openai --api-key sk-fake
{"data":{"baseUrl":null,"error":null,"latencyMs":100,"provider":"openai","valid":false},...}
```
`valid: false` nhưng `error: null` → không biết tại sao fail (401? network? unknown provider?). Phải trả thông tin gốc từ upstream.

`translator translate`:
```bash
$ echo '{"model":"gpt-4o","messages":[...]}' | \
  openproxy --robot translator translate --from openai --to claude
{"data":{"error":"provider and model required","success":false},...}
```

Help nói `--model [default: ""]` (tức là optional). Nhưng thực tế nếu không truyền `--model` thì translator báo "model required". → default `""` và error là `required` là mâu thuẫn. Phải sửa default hoặc đổi message.

---

### Bug 15 — `web/scripts/build-npm-package.mjs` thiếu (TRUNG BÌNH)

`web/package.json`:
```json
"scripts": {
  "package:npm":  "node scripts/build-npm-package.mjs",
  "dist:npm":     "cargo build --release && pnpm run build && node scripts/build-npm-package.mjs",
  "verify:npm":   "node scripts/verify-npm-package.mjs"
}
```

Trong khi `web/scripts/` chỉ có:
```
run-source-stack.mjs
```

→ 3 npm script trên đều gãy. Nếu pipeline release cũng dựa vào `dist:npm` thì việc publish lên npm sẽ fail (có thể là lý do `@openprx/openproxy` chưa được publish ở registry — Bug 16 / SKILL.md đã liệt kê "`npm install -g @openprx/openproxy` → E404").

---

### Bug 16 — Release v0.1.0 build từ **stub dashboard** (BLOCKER, root cause Bug 1)

`build.rs` của repo gate compile:
```rust
let dist = std::path::Path::new("web/dist/index.html");
if !dist.exists() { panic!("web/dist not built. Run: (cd web && pnpm install --frozen-lockfile && pnpm run build)"); }
```

Nhưng release v0.1.0 đã built với `web/dist/index.html` **chỉ chứa 147 byte meta-refresh stub** (xem Bug 1). Có 2 khả năng:
1. Pipeline release chạy `pnpm run build` nhưng Astro với `format: 'file'` không sinh ra `dashboard/index.html` mà sinh `dashboard.html` → Rust embed không có cách serve trên `/dashboard` (root cause của Bug 1).
2. Hoặc pipeline release skip `pnpm run build` và chỉ build với một stub `index.html` hand-written.

Cần audit `.github/workflows/release.yml` xem bước nào build dashboard, kiểm tra `pnpm run build` thật sự được chạy không và `web/dist/` cuối cùng chứa gì trước khi `cargo build --release`.

---

## Những thứ chạy OK ✓ (mặc dù không hoàn hảo)

- `install.sh` (curl one-liner) — chạy, verify checksum, drop binary `~/.local/bin/openproxy`, install SKILL.md.
- `openproxy --help` — đầy đủ subcommand.
- `openproxy server start --detach` (không kèm `--no-open`) — server bật, log OK.
- `GET /health` và `GET /api/health` — `200 {"status":"ok"}` / `200 {"ok":true}`.
- `POST /api/auth/login` với `123456` — `200 {"success":true}` + cookie.
- `GET /v1/models` với bearer hợp lệ (sau restart) — trả 66 model built-in.
- `openproxy --robot server status / doctor / db init / db export / settings get / key list / key add / key delete / combo create / combo list / models list / provider list / pool list / quota list / usage summary / logs tail / logs stats / mitm status / tunnel status / completion bash / schema list / schema example <res> / translator formats / media providers list / tool list / provider validate / route` — tất cả đều trả JSON envelope hợp lệ `openproxy.v1.*`.
- Sau khi tìm ra path `/dashboard.html`, SPA load đầy đủ (sidebar, API Endpoint, Token Saver, API Keys, Providers, Combos, Usage, Quota Tracker, MITM, CLI Tools, Media Providers, Proxy Pools, Console Log, Settings).
- Dark-mode toggle render đúng, danh sách model trong API render đúng.

---

## Đề xuất ưu tiên fix

1. **(Blocker)** Fix vòng lặp redirect dashboard: hoặc đổi Astro `build.format: 'directory'`, hoặc sửa `serve_embedded` thử `<path>.html` trước khi fallback `index.html`.
2. **(Cao)** Thêm `--version` cho root CLI và đồng bộ với version trong `Cargo.toml` / GitHub release tag.
3. **(Cao, bảo mật)** Đổi default `--host` về `127.0.0.1` cho khớp README. Cân nhắc rename env `HOST` → `HOSTNAME` để khớp README, hoặc cập nhật README ghi `HOST`.
4. **(Cao)** Mỗi lần data dir trống (apiKeys=[]) hãy mint admin key idempotently thay vì lỗi `conflict`. Hoặc lần đầu server start tự sinh admin key, log ra `~/.openproxy/admin.key` chmod 600.
5. **(Cao)** Sửa `chat send --prompt` — coi giá trị là **string** (không phải file path); thêm flag `--prompt-file <path>` riêng nếu cần đọc từ file.
6. **(Trung bình)** Đồng bộ README, SKILL.md với CLI thật cho: `key add`, `combo create`, `provider add`, `quota`, `usage`, vị trí flag `--no-open`. Một AI agent đi theo SKILL.md hiện tại sẽ fail nhiều bước.
7. **(Trung bình)** `doctor` không có side-effect, exit code đúng.
8. **(Trung bình)** Khi server đang chạy, `key add` CLI nên đi qua HTTP API thay vì viết trực tiếp `db.json` để tránh race window.
9. **(Trung bình)** Trả error message thật trong `provider validate` và bỏ default `""` cho `translator translate --model` (hoặc bỏ "required" trong server-side).
10. **(Trung bình)** Bổ sung `web/scripts/build-npm-package.mjs` và `verify-npm-package.mjs`, hoặc xoá các script không dùng được trong `package.json`.

---

## Phụ lục — Lệnh đã chạy

Toàn bộ thử nghiệm chạy trên `openproxy v0.1.0` (Linux x86_64) trong session này, có thể reproduce bằng:

```bash
curl -fsSL "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh" | bash
export PATH="$HOME/.local/bin:$PATH"

openproxy --version                       # Bug 2
openproxy --help                          # OK
openproxy doctor                          # Bug 5
openproxy --robot server init             # Bug 4
openproxy server start --detach --no-open # Bug 6
openproxy --no-open server start --detach # OK
curl -i http://127.0.0.1:4623/            # Bug 1 (redirect stub)
curl -i http://127.0.0.1:4623/dashboard   # Bug 1
curl -i http://127.0.0.1:4623/dashboard.html  # OK (SPA load)
openproxy key add admin-test              # Bug 7
openproxy --robot key add k1 secret123    # OK, nhưng Bug 13 (race)
openproxy --robot provider add openai-test '{...}'  # Bug 8
openproxy combo create my-stack --models ...  # Bug 9
openproxy --robot combo create --name my-stack --models ...  # OK
openproxy quota                           # Bug 10
openproxy chat send --model my-stack --prompt "hello"  # Bug 12
openproxy --robot settings version        # Bug 11
openproxy --robot provider validate --provider openai --api-key sk-fake # Bug 14
echo '{...}' | openproxy --robot translator translate --from openai --to claude # Bug 14
```
