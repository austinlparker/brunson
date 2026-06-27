# AGENTS.md — brunson Daemon API for AI Agents

The brunson daemon serves a local HTTP API at `http://127.0.0.1:17890` (configurable via `[daemon] port`). This is the primary integration point for AI agents to query PR data, trigger refreshes, and re-classify PRs.

## Quick Start for Agents

```bash
# Check daemon health
curl -s http://localhost:17890/health | jq

# Get all PRs grouped by state
curl -s http://localhost:17890/prs | jq

# Get a specific PR's full detail
curl -s http://localhost:17890/prs/myorg~myrepo~123 | jq

# Get the raw diff for a PR
curl -s http://localhost:17890/prs/myorg~myrepo~123/diff

# Force a refresh (re-poll GitHub), then wait and re-query
curl -s -X POST http://localhost:17890/prs/refresh
sleep 3
curl -s http://localhost:17890/prs | jq

# Re-run LLM classification on a PR
curl -s -X POST http://localhost:17890/prs/myorg~myrepo~123/classify
```

## PR ID Format

PRs are identified by a URL-safe slug: `{owner}~{repo}~{number}`

Example: `myorg~myrepo~123` for `github.com/myorg/myrepo/pull/123`

## Common Agent Tasks

### "What needs my attention?"

```bash
# Authored PRs where the next action is yours: respond, fix CI, or address feedback
curl -s http://localhost:17890/prs | jq '.groups.authored_action_needed'

# PRs waiting on your review
curl -s http://localhost:17890/prs | jq '.groups.review_needed'
```

### "What changed after I reviewed?"

```bash
curl -s http://localhost:17890/prs | jq '.groups.review_update'
```

### "What am I ready to merge?"

```bash
curl -s http://localhost:17890/prs | jq '.groups.authored_ready_to_merge'
```

### "Show me the diff for PR X"

```bash
curl -s http://localhost:17890/prs/myorg~myrepo~42/diff
```

### "Refresh all data before I analyze"

```bash
curl -s -X POST http://localhost:17890/prs/refresh
# Poll health until refresh_in_progress is false
curl -s http://localhost:17890/health | jq .refresh_in_progress
```

## API Endpoints Reference

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Daemon status, current user, rate limit, poll state |
| GET | `/prs` | All PRs grouped by state |
| GET | `/prs/{id}` | Full PR detail (checks, comments, files) |
| GET | `/prs/{id}/diff` | Raw unified diff |
| POST | `/prs/refresh` | Trigger immediate GitHub poll (returns 202) |
| POST | `/prs/{id}/classify` | Re-run LLM classification (returns 202 or 503) |
| GET | `/config` | Effective config (no secrets) |

## TUI Layout (Brunson)

`brunson tui` renders a five-blade, Xbox 360-style horizontal dashboard. The active blade expands while the other four collapse to thin vertical strips.

| # | Blade | Accent | Content |
|---|-------|--------|---------|
| 1 | Inbox | blue | Triages PRs into **OPENED BY ME** and **NEEDS MY REVIEW**, sorted by priority |
| 2 | Overview | peach | PR header, stat tiles, summary/description, checks, last activity |
| 3 | Activity | mauve | Connected vertical timeline of PR events |
| 4 | Files | green | Changed-file picker with status, additions, deletions |
| 5 | Diff | teal | Unified diff with two-number gutter and inline review comments |

Navigation: `←/h` back, `→/l/Enter` deeper, `1`–`5` jump, `j/k`/`↑↓` scroll, `R` refresh, `q` quit.

The status line at the bottom shows the selected PR title, current blade, and a block cursor; the keybar below it lists bindings. Press `o` to open the selected PR in a browser. (PR titles and file paths are intentionally not rendered as OSC 8 hyperlinks — doing so via `Cell::set_symbol` corrupts ratatui 0.30's cell-width calculation and breaks the selection highlight.)

Inside the Inbox, the daemon's eight internal `PrGroup` values are folded into exactly two display sections:

- **OPENED BY ME** = `authored_action_needed`, `authored_ready_to_merge`, `authored_waiting`, plus `draft` PRs authored by the current user.
- **NEEDS MY REVIEW** = `review_needed`, `review_update`, `review_done`, plus `draft` PRs by others and `other` PRs.

## TUI Rendering Architecture

The TUI is built from a small tree of reusable, layout-first components under `src/tui/render/`:

- **`Component` trait** (`component.rs`) — pure renderers that take a read-only `RenderContext<'a> { state: &AppState, view: &ViewState, theme: &Theme }`. Leaf renderers never mutate `AppState`; all scroll clamping happens in `ViewStateManager::prepare` before the frame is drawn.
- **`RootLayout`** (`layout.rs`) — owns the outer geometry (body / command line / keybar) and the five-blade horizontal split, plus the 50×12 minimum-size splash. Produces a `ViewLayout` every child reads. `Blade` is defined here.
- **`ScrollViewport`** (`primitives.rs`) — one reusable virtualized list used by Inbox, Files, Activity, Diff, and Overview section bodies. Handles windowing, scrollbar, and offset slicing over a flattened `&[Line]`.
- **`InlineToast`** (`chrome.rs`) — centered overlay for errors and action-stub feedback; the keybar is never replaced by an error string.

State is split: domain/cache data lives in `AppState` (`src/tui/app.rs`); transient navigation, focus, and scroll state lives in `ViewState`/`ViewStateManager` (`src/tui/state.rs`). `ViewStateManager::prepare` is the single place that recomputes the flat PR list, reconciles selection, and clamps scroll offsets each frame. Expensive markdown/diff parsing is cached in `RenderCache` (`render/cache.rs`) so it does not run every frame.

Every component fully paints its allocated area (via `fill`/`Surface`), so no manual buffer-write hacks (`clear_area`, `CellDiffOption`/`skip` resets) remain. The blade view modules (`src/tui/views/{inbox,overview,activity,files,diff}.rs`) are thin compositions over these primitives. PR/file titles are plain styled text (not hyperlinks) — see the note above about ratatui 0.30 cell-width corruption.

## Response Formats

### Health

```json
{
  "service": "brunson",
  "version": "0.1.0",
  "status": "ok",
  "current_user": "yourname",
  "last_poll_at": "2024-06-24T12:00:00Z",
  "last_poll_error": null,
  "rate_limit_remaining": 4998,
  "refresh_in_progress": false
}
```

### PR List

```json
{
  "groups": {
    "review_needed": [
      {
        "id": "myorg~myrepo~123",
        "number": 123,
        "title": "Add feature X",
        "author": "coworker",
        "group": "review_needed",
        "next_action": "Review now",
        "check_status": "pending",
        "llm_priority": "high",
        "url": "https://github.com/myorg/myrepo/pull/123"
      }
    ],
    "authored_ready_to_merge": [...]
  },
  "updated_at": "2024-06-24T12:00:00Z"
}
```

### Group Keys

Authored lane: `authored_action_needed`, `authored_ready_to_merge`, `authored_waiting`

Review-requested lane: `review_needed`, `review_update`, `review_done`

Other: `draft`, `other`

Each PR summary includes `next_action`, a daemon-computed action label such as `Review now`, `Re-review`, `Respond`, `Fix CI`, `Address feedback`, `Merge`, or `Waiting`.

Full PR detail responses include a `timeline` array of activity events (`comment`, `review`, `commit`, `force_push`, `review_requested`, etc.) with `actor`, `created_at`, and `detail` fields. For `comment` and `review` events, `detail` contains the **full** comment/review body so it can be rendered with Markdown formatting in the TUI.
