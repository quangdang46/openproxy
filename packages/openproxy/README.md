# openproxy

Single-binary AI router with embedded web dashboard. Routes Claude Code, Codex, Cursor, Cline, OpenClaw, and other AI CLI tools to 40+ providers with auto-fallback, quota tracking, and 20–40% token savings via RTK.

## Install

```bash
npm install -g @openprx/openproxy
```

This package is a thin shim that downloads no scripts at install time. The actual binary is delivered via the platform-specific optional dependencies:

- `@openprx/openproxy-linux-x64`
- `@openprx/openproxy-linux-arm64`
- `@openprx/openproxy-darwin-x64`
- `@openprx/openproxy-darwin-arm64`

Your package manager installs only the one matching your platform.

## Usage

```bash
openproxy            # start server, auto-open browser at http://127.0.0.1:4623
openproxy --help     # full CLI reference
openproxy --no-open  # start server without opening a browser (headless / SSH / containers)
```

## Alternative install methods

- One-shot curl installer:
  ```bash
  curl -fsSL "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh" | bash
  ```
- GitHub Releases (manual): https://github.com/quangdang46/openproxy/releases
- Build from source: https://github.com/quangdang46/openproxy#building-from-source

## Documentation

Full README, supported providers, and configuration: https://github.com/quangdang46/openproxy

## License

MIT
