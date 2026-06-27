# brunson — Terminal PR Manager

A Ratatui-based TUI that surfaces GitHub PRs grouped by actionable state, lets you browse diffs and comments in-terminal, and optionally uses a local LLM (via LM Studio) to classify/prioritize PRs.

## Architecture

**Daemon/TUI split:** A long-running daemon (`brunson daemon`) owns GitHub polling, caching, and LLM classification, exposing a local HTTP API on `127.0.0.1`. The TUI (`brunson tui`) is a thin client. AI agents (or any HTTP client) can query the same API independently.

```
┌─────────────┐     HTTP/JSON     ┌──────────────────┐
│  brunson     │ ◄──────────────► │  brunson         │
│  tui         │   127.0.0.1      │  daemon          │
└─────────────┘                  │  ├─ GitHub poll  │
                                  │  ├─ PR store     │
┌─────────────┐     HTTP/JSON     │  ├─ LLM classify │
│  AI agent    │ ◄──────────────► │  └─ axum server  │
│  (curl)      │                  └──────────────────┘
└─────────────┘
```

## Prerequisites

1. **Rust** (edition 2021, stable)
2. **GitHub CLI** (`gh`) — authenticated via `gh auth login`, or set `GH_TOKEN` / `GITHUB_TOKEN`

```bash
# Authenticate with GitHub
gh auth login

# Verify
gh auth token
```

## Installation

### Homebrew (recommended)

```bash
brew tap austinlparker/brunson
brew install brunson
```

### From source

```bash
cargo install --path .
```

## Quick Start

```bash
# Initialize config with defaults
brunson init

# Edit config to watch your repos
vim ~/.config/brunson/config.toml

# Start the daemon (runs in foreground)
brunson daemon

# In another terminal, start the TUI
brunson tui
```

The TUI will auto-detect or spawn the daemon if it's not running.

## Configuration

Config lives at `~/.config/brunson/config.toml` (or `$XDG_CONFIG_HOME/brunson/config.toml`).

```toml
[github]
watch = ["myorg", "myorg/important-repo"]  # empty = all PRs involving you
poll_interval = 300                          # seconds

[daemon]
port = 17890              # local HTTP API port
kill_on_tui_exit = false  # kill spawned daemon on TUI exit

[llm]
enabled = false                          # enable LLM classification
endpoint = "http://localhost:1234/v1"    # LM Studio endpoint
model = ""                               # empty = auto-detect
classify_on_change = true
max_output_tokens = 4096  # increase for reasoning-heavy local models

[tui]
diff_style = "unified"       # or "side-by-side"
show_line_numbers = true
osc8_links = true            # emit OSC 8 terminal hyperlinks for PR/file titles
```

Set `osc8_links = false` if your terminal emulator misrenders OSC 8 hyperlinks or you prefer plain text. The `o` key always opens the selected PR in a browser regardless of this setting.

### GitHub Enterprise

Set `GH_HOST` to your GHES hostname:

```bash
export GH_HOST=github.company.com
```

## Key Bindings

The TUI uses a five-blade horizontal dashboard (Inbox → Overview → Activity → Files → Diff). The focused blade expands while the others collapse to thin strips.

| Key | Action |
|-----|--------|
| `←`/`h` | Back one blade |
| `→`/`l` / `Enter` | Forward one blade |
| `1`–`5` | Jump to a blade directly |
| `j`/`↓` / `k`/`↑` | Scroll / move selection within the active blade |
| `Space` | Toggle Inbox group collapse |
| `a` | Approve stub |
| `r` | Request changes stub |
| `m` | Merge stub |
| `o` | Open selected PR in browser |
| `R` | Refresh from GitHub |
| `/` | Search stub |
| `?` | Show help |
| `q` | Quit |

### Diff Blade

| Key | Action |
|-----|--------|
| `j`/`k` | Scroll one line |
| `Ctrl-d`/`Ctrl-u` | Half-page scroll |
| `g`/`G` | Top / bottom |
| `f`/`Tab` | Jump to next file |
| `Shift-Tab` | Jump to previous file |
| `n` | Toggle line numbers |

## Daemon API Reference

The daemon serves a local REST API on `127.0.0.1:{port}`. All responses are JSON.

### `GET /health`

Daemon health and status.

```bash
curl http://localhost:17890/health
```

```json
{
  "service": "brunson",
  "version": "0.1.0",
  "status": "ok",
  "current_user": "yourname",
  "last_poll_at": "2024-06-24T12:00:00Z",
  "rate_limit_remaining": 4998,
  "refresh_in_progress": false
}
```

### `GET /prs`

All tracked PRs grouped by actionable state.

```bash
curl http://localhost:17890/prs
```

### `GET /prs/{id}`

Full PR detail. `{id}` is `{owner}~{repo}~{number}`.

```bash
curl http://localhost:17890/prs/myorg~myrepo~123
```

### `GET /prs/{id}/diff`

Raw unified diff text.

```bash
curl http://localhost:17890/prs/myorg~myrepo~123/diff
```

### `POST /prs/refresh`

Trigger an immediate GitHub poll cycle.

```bash
curl -X POST http://localhost:17890/prs/refresh
```

### `POST /prs/{id}/classify`

Re-run LLM classification on a PR (requires LLM enabled).

```bash
curl -X POST http://localhost:17890/prs/myorg~myrepo~123/classify
```

### `GET /config`

Current effective configuration (sanitized, no secrets).

## PR Inbox Groups

PRs are grouped into a priority inbox with two main lanes: PRs you authored and PRs where review is requested from you. Each PR appears in exactly one group, and summaries include a daemon-computed `next_action` label such as `Review now`, `Re-review`, `Respond`, `Fix CI`, `Address feedback`, `Merge`, or `Waiting`.

| Group key | Description |
|-----------|-------------|
| `authored_action_needed` | Your PR needs your action: CI failed, changes were requested, or someone commented after your last response |
| `authored_ready_to_merge` | Your PR is approved, CI passes, and it is mergeable |
| `authored_waiting` | Your PR is waiting on reviewers or CI; nothing obvious for you to do right now |
| `review_needed` | Review is requested from you and you have not reviewed yet |
| `review_update` | You reviewed already, but new commits or force-pushes landed afterward |
| `review_done` | You reviewed and there is no newer activity requiring re-review |
| `draft` | PR is a draft |
| `other` | Involved PRs outside the authored/review-requested lanes |

Full PR detail responses include a `timeline` array of PR activity events (`comment`, `review`, `commit`, `force_push`, `review_requested`, etc.) for rendering the activity stream in the TUI.

## LLM Classification

When `[llm] enabled = true`, the daemon sends PR context to a local LM Studio instance for priority classification. The classification result (priority: high/medium/low + summary) appears in the TUI and the `/prs` API response.

1. Install and start [LM Studio](https://lmstudio.ai/)
2. Load a model
3. Enable in config:

```toml
[llm]
enabled = true
endpoint = "http://localhost:1234/v1"
model = ""  # auto-detects first available model
max_output_tokens = 4096  # increase for reasoning-heavy local models
```

## Troubleshooting

- **"No GitHub token found"**: Run `gh auth login` or set `GH_TOKEN`.
- **"Daemon did not become ready"**: Check `~/.local/share/brunson/daemon.log`.
- **"Port already in use"**: Another daemon may be running. Check `~/.local/share/brunson/daemon.pid`.
- **Rate limit warnings**: Increase `poll_interval` in config.

## License

MIT
