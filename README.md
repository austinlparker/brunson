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
# Start the TUI. It auto-spawns the daemon if needed, and on first run it
# opens an interactive setup wizard (press 'w' to reopen it any time).
brunson tui

# Non-interactive / agent install: ensure config dir and default config exist
brunson setup --yes

# Agent-readable setup summary (readiness, advice, and config prompts)
brunson setup --yes --json

# Or run the daemon on its own (foreground)
brunson daemon
```

Interactive setup lives entirely in the TUI wizard; `brunson setup` without `--yes`/`--json` exits with an error pointing you there. After editing the config by hand, POST `/config/reload` to a running daemon (or restart it) so the changes take effect.

## Configuration

Config lives at `~/.config/brunson/config.toml` (or `$XDG_CONFIG_HOME/brunson/config.toml`).

```toml
[github]
watch = []          # shorthand: empty = all PRs involving you
poll_interval = 300 # seconds

# Precise targeting. With watch = [], only these targets are searched.
[[github.targets]]
repo = "myorg/important-repo"
direct_review_requests = true          # direct requests for @me only
team_review_requests = ["myorg/team"]  # only these team review requests
include_authored = true
include_involved = false

[daemon]
port = 17890              # local HTTP API port
kill_on_tui_exit = false  # kill spawned daemon on TUI exit

[llm]
enabled = false                          # enable LLM classification
provider = "lm_studio"                   # "lm_studio" or "openai_compatible"
endpoint = ""                            # leave empty for provider-specific defaults
api_key = ""                             # required for most OpenAI-compatible endpoints
model = ""                               # empty = auto-detect
classify_on_change = true
max_output_tokens = 4096  # increase for reasoning-heavy local models

[tui]
show_line_numbers = true
```

Explicit `team_review_requests` entries should use GitHub team slugs (`org/team-slug`). Brunson keeps PRs found only through those team targets only while the PR still requests that team and the authenticated viewer is still a current member of that team; if GitHub cannot answer the membership check, the refresh fails safely and preserves the existing PR list instead of dropping data.

The `o` key opens the selected PR in a browser.


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
| `/` | Filter inbox |
| `o` | Open selected PR in browser |
| `R` | Refresh from GitHub |
| `w` | Open the setup wizard (also opens automatically on first run) |
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

Daemon health and status. Includes setup diagnostics so agents can tell whether the daemon is ready to serve PR data.

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
  "refresh_in_progress": false,
  "setup_status": "ready",
  "setup_message": null
}
```

### `GET /setup/status`

Machine-readable setup diagnostics. Returns `ready`, `status` (`missing_config`, `missing_auth`, `llm_misconfigured`, or `ready`), GitHub auth state, LLM reachability, and actionable `next_steps`.

```bash
curl http://localhost:17890/setup/status
```

### `GET /config/preview`

Show the GitHub search queries generated from the current effective config.

```bash
curl http://localhost:17890/config/preview
```

### `POST /config/validate`

Validate a proposed config JSON payload and preview the generated GitHub search queries.

```bash
curl -X POST http://localhost:17890/config/validate
```

### `POST /config/reload`

Re-parse `config.toml` from disk and apply all changes (poll interval, watch list, LLM config, etc.) without restarting the daemon.

```bash
curl -X POST http://localhost:17890/config/reload
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

When `[llm] enabled = true`, the daemon sends PR context to an OpenAI-compatible endpoint for priority classification. The classification result (priority: high/medium/low + summary) appears in the TUI and the `/prs` API response.

### LM Studio (default)

1. Install and start [LM Studio](https://lmstudio.ai/)
2. Load a model and start the local server
3. Enable in config:

```toml
[llm]
enabled = true
provider = "lm_studio"
endpoint = ""  # defaults to http://localhost:1234/v1
api_key = ""   # optional when LM Studio does not require auth
model = ""     # auto-detects first available model
max_output_tokens = 4096
```

### OpenAI-compatible providers (OpenAI, Azure, Ollama, etc.)

```toml
[llm]
enabled = true
provider = "openai_compatible"
endpoint = "https://api.openai.com/v1"  # or your proxy / Azure endpoint
api_key = "$OPENAI_API_KEY"
model = "gpt-4o-mini"
max_output_tokens = 4096
```

The `api_key` is sent as `Authorization: Bearer {api_key}` on every request.

## Troubleshooting

- **"No GitHub token found"**: Run `gh auth login` or set `GH_TOKEN`.
- **"Daemon did not become ready"**: Check `~/.local/share/brunson/daemon.log`.
- **"Port already in use"**: Another daemon may be running. Check `~/.local/share/brunson/daemon.pid`.
- **Rate limit warnings**: Increase `poll_interval` in config.

## License

MIT
