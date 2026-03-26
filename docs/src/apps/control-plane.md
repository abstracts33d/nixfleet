# Dashboard

## Purpose

Single Go binary serving documentation, security audit dashboard, system monitor, and graph explorer on `http://localhost:3000` with live reload via WebSocket.

Replaces the separate `nix run .#docs` and `nix run .#docs-guide` commands with a unified interface.

## Usage

```sh
nix run .#dashboard
```

Opens at `http://localhost:3000`.

## Routes

| Route | Description |
|-------|-------------|
| `/` | Landing page with status badges and quick links |
| `/docs/` | Technical reference (mdbook) |
| `/guide/` | Conceptual guide (mdbook) |
| `/audits/` | Security audit dashboard (D3.js timeline, severity chart, trend) |
| `/monitor/` | System monitor (hosts, inputs freshness, test coverage) |
| `/graph/` | Graph explorer (module deps, flake inputs, automation flow) |
| `/ws` | WebSocket endpoint for live reload |

## API Endpoints

All return `application/json`.

| Endpoint | Data |
|----------|------|
| `GET /api/status` | Branch, last commit, dirty file count, flake.lock age |
| `GET /api/hosts` | Host names, platforms, hostSpec flags (parsed from `fleet.nix`) |
| `GET /api/tests` | Eval and VM test names (static list) |
| `GET /api/audits` | Security review dates, severity counts, resolved/unresolved |
| `GET /api/inputs` | Flake inputs with URL, rev, age in days (parsed from flake.lock) |
| `GET /api/graph` | Module nodes with types, host-scope edges, input follows, hooks/skills flow |
| `GET /audits/render?file=<name>` | Renders a security review markdown file to HTML |

## CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `3000` | Server port |
| `--project-root` | `.` | Repository root path |
| `--docs-dir` | (required) | Path to mdbook docs build output |
| `--guide-dir` | (required) | Path to mdbook guide build output |
| `--audits-dir` | `""` | Path to security reviews directory |
| `--watch-dirs` | `""` | Comma-separated directories to watch for changes |

## Architecture

```
dashboard/
├── main.go         # HTTP server, route setup, embedded static files
├── api.go          # /api/* handlers (git exec, file parsing)
├── markdown.go     # goldmark markdown-to-HTML for audit rendering
├── websocket.go    # WebSocket hub (upgrade, broadcast, cleanup)
├── watcher.go      # fsnotify file watcher with 500ms debounce
├── static/
│   ├── index.html
│   ├── audits.html
│   ├── monitor.html
│   ├── graph.html
│   ├── style.css
│   └── js/
│       ├── audits.js
│       ├── monitor.js
│       ├── graph.js
│       └── livereload.js
└── go.mod
```

## WebSocket Protocol

The server broadcasts JSON messages to all connected clients when watched files change:

```json
{"type": "reload", "path": "modules/core/nixos.nix"}
```

The client (`livereload.js`) reloads the page on any message. Auto-reconnects with exponential backoff (1s to 30s).

## Nix Integration

The `nix run .#dashboard` app script:
1. Builds both mdbook sites into `.dashboard/`
2. Launches the Go binary with all flags configured
3. Watches `docs/`, `modules/`, `.claude/` for live reload

The Go binary is built with `pkgs.buildGoModule` in `modules/apps.nix`.

## Security

- Binds to `127.0.0.1` only (localhost)
- No authentication (local dev tool)
- Path traversal protection on audit file rendering
- All `os/exec` calls use explicit argument arrays (no shell)
- Never exposes secrets, nix store passwords, or `.keys/` contents
