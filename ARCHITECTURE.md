# cos — chief of staff

Tracks GitHub links you're involved in, commitments you make, and asks/mentions
directed at you across Slack and Linear, and surfaces them in a small local
dashboard.

## Scope (v1)

- Capture: Slack (user-token polling) + Linear (personal API key polling) +
  GitHub (status sync for tracked links)
- Extraction: Claude, given tool calls that write directly to SQLite
- Storage: SQLite, single file
- UI: simple CRUD dashboard — view, edit status, resolve, delete

No real-time push in v1. Everything is a poll loop on a schedule.

## System overview

```mermaid
flowchart LR
    subgraph Sources
        SL[Slack search.messages / reactions.list]
        LN[Linear GraphQL API]
        GH[GitHub REST API]
    end

    subgraph Ingest["Poll loop (cron / scheduled task)"]
        P[Poller] --> EX[Claude extraction call]
    end

    SL --> P
    LN --> P
    GH -->|status sync for tracked links| P

    EX -->|tool calls: log_github_link,\nlog_commitment, log_mention,\nlog_linear_ticket, resolve| DB[(SQLite)]

    subgraph App["Rust / axum server"]
        API[axum handlers]
        HTMX[htmx partials]
        PREACT["Preact + signals\n(isolated interactive islands)"]
    end

    DB <--> API
    API --> HTMX
    HTMX -.->|hydrates| PREACT
    Browser((Browser)) <--> API
```

## Ingestion sequence

```mermaid
sequenceDiagram
    participant Cron as Scheduler
    participant Poll as Poller
    participant Slack as Slack API
    participant Linear as Linear API
    participant Claude as Claude (tool-use)
    participant DB as SQLite

    Cron->>Poll: tick (e.g. every 5 min)
    Poll->>Slack: search.messages / reactions.list (since last_synced)
    Poll->>Linear: viewer.notifications / assignedIssues (since last_synced)
    Poll->>Claude: raw messages + tool defs
    Claude-->>DB: log_github_link / log_commitment / log_mention / log_linear_ticket
    Note over Claude,DB: Claude decides relevance;<br/>no-op if nothing worth logging
    Poll->>DB: update last_synced cursor
```

## Data model

```mermaid
erDiagram
    GITHUB_LINKS {
        text url PK
        text repo
        text relationship
        text status
        text context
        text source_permalink
        text first_seen
        text last_synced
    }
    COMMITMENTS {
        integer id PK
        text text
        text due_date
        text status
        text source_permalink
        text created_at
    }
    MENTIONS {
        text source_permalink PK
        text from_user
        text text
        boolean needs_response
        boolean responded
        text created_at
    }
    LINEAR_TICKETS {
        text identifier PK
        text title
        text state
        text relationship
        text linked_github_url
        text last_synced
    }
    INITIATIVES {
        integer id PK
        text name
        text description
        text created_at
    }
    GITHUB_LINKS ||--o| LINEAR_TICKETS : "linked_github_url"
    INITIATIVES ||--o{ GITHUB_LINKS : "initiative_id"
    INITIATIVES ||--o{ COMMITMENTS : "initiative_id"
    INITIATIVES ||--o{ MENTIONS : "initiative_id"
    INITIATIVES ||--o{ LINEAR_TICKETS : "initiative_id"
```

Every item table (`github_links`, `commitments`, `mentions`, `linear_tickets`)
carries a nullable `initiative_id`. One initiative per item, not a
many-to-many tag table — simplest thing that answers "what's open under this
initiative," and cheap to widen into a join table later if an item ever
genuinely needs to belong to more than one.

**State propagation.** `linear_tickets.linked_github_url` is the one
structural (non-text-guessed) relationship in the schema, so it's the one
place state changes cascade: when `log_linear_ticket` sees a ticket land on
`Done` with a `linked_github_url` set, it resolves the matching `github_links`
row (`db.rs`). `resolved` is treated as sticky everywhere else — both the
manual Resolve button and this cascade set it, and `sync_github_status` now
guards `WHERE status != 'resolved'` so a later "live" GitHub state sync can't
silently revert it (this was a latent bug independent of the cascade — found
while wiring the cascade in, since without the guard the two would have
fought each other within the same extraction cycle). Commitments and mentions
have no equivalent structural FK to a specific PR/ticket, so no cascade
reaches them — the ticket→PR link is the only one that's a real foreign key
rather than a text match.

**Linear scope.** Every Linear notification still gets ingested unconditionally
(filtering at ingest time would be premature — you need to classify an item
before you can know whether it relates to anything). But a ticket that's been
reviewed and is still standalone (no initiative) has no Slack/PR activity to
cross-reference — that's just Linear's own task tracking, which Linear
already shows you — so `/linear-tickets` collapses those into a closed
`<details>` section instead of duplicating Linear's UI. Pending
(not-yet-reviewed) tickets are unaffected: "no initiative yet" isn't the same
as "judged unrelated."

## Initiatives

The cross-cutting grouping a PR, a Slack ask, and a Linear ticket can all
belong to ("the ledger dedupe fix"), so you can look at one thing instead of
four lists.

Classification is deliberately a separate, later stage from extraction — see
Polling below for why.

```mermaid
flowchart LR
    Msg["new Slack message\n(since cursor)"] --> AE["agent_extraction::run_messages\n(Sonnet 5, per message)"]
    AE -->|log_github_link / log_commitment / flag_mention\nNO initiative assigned| DB[(SQLite)]
    DB --> Pending["pending items\n(initiative_reviewed_at IS NULL)"]
    DB --> Standalone["recent standalone items\n(reviewed, no initiative, last 7 days)\noptional reconsideration context"]
    Pending --> ABC["agent_bulk_classify::run\n(Opus 5, ONE call for the whole batch)"]
    Standalone --> ABC
    ABC -->|assign_initiative_item x N,\nonly for items worth grouping| DB
    DB --> List["GET /initiatives\n(cards: name, open/total)"]
    DB --> Detail["GET /initiatives/:id\n(all 4 item types, filtered)"]
```

Two parallel implementations, chosen at runtime by whether `ANTHROPIC_API_KEY`
is set (`agent::AnthropicClient::is_configured`):

- **No key**: `extraction.rs` (regex/keyword rules) + `classify.rs` (group by
  shared GitHub repo, one item at a time). Deterministic, free, no network
  call — what runs by default and in CI. Doesn't make "is this worth an
  initiative" judgment calls — it either finds a repo match or leaves the
  item alone forever (not standalone-pending-review, since the heuristic
  path doesn't participate in that state machine at all; the agent path
  picks it up correctly the moment a key is configured, since unreviewed
  just means `initiative_reviewed_at IS NULL`).
- **Key present**: `agent_extraction.rs` (triage) + `agent_bulk_classify.rs`
  (initiative assignment), real `POST /v1/messages` tool-use calls (raw HTTP
  via `reqwest`; there's no official Anthropic Rust SDK) — but at two
  different model tiers for two different jobs:
  - `agent_extraction`: **Sonnet 5**, `effort: "low"`, one call per Slack
    message. Decides PR relationship (authored / reviewing / mentioned),
    whether it's a commitment, whether it needs a response. Does **not**
    decide initiative — that's deferred entirely to the bulk pass.
  - `agent_bulk_classify`: **Opus 5**, `effort: "high"`, one call for the
    entire batch of pending items across all four tables. Every pending item
    is listed in the prompt; Opus responds with one `assign_initiative_item`
    tool call per item **it decides warrants one** — not every item gets a
    group. A small, isolated ask with nothing else related to it is left
    standalone on purpose; the system prompt explicitly says not to invent an
    initiative just to give every item a home. Seeing the whole batch at once
    is what lets it consolidate items that only make sense together (e.g. an
    infra PR, two unrelated-looking Slack mentions, and a Linear ticket all
    turned out to be the same "Release Cut Readiness" effort) in a way a
    one-item-at-a-time pass structurally can't — a prior per-item-Sonnet
    version of this classifier split that same set into two initiatives; the
    bulk Opus pass merged them correctly. Skipped entirely (no API call) when
    nothing is pending.

**Pending vs. standalone** — `initiative_id` alone can't distinguish "never
looked at this" from "looked, and it's genuinely fine on its own," so there's
a second column, `initiative_reviewed_at`, set by `assign_*` every time a
decision is made regardless of outcome:

| `initiative_id` | `initiative_reviewed_at` | Meaning |
|---|---|---|
| NULL | NULL | Pending — must get a decision next bulk cycle |
| NULL | set | Reviewed, deliberately left standalone |
| set | set | Reviewed, assigned |

Every bulk cycle, pending items are mandatory — each either gets assigned or
gets `initiative_reviewed_at` stamped with no `initiative_id` (a real
decision, not a skip). Items already reviewed and still standalone, but seen
within the last 7 days, are re-offered as a second, clearly-labeled "optional
reconsideration" list in the same prompt — not because they need another
look on a timer, but so that a *new* item arriving later can retroactively
pull one of them into a freshly-formed initiative (e.g. a one-off mention
from last week turns out to be the first sign of something that only became
a pattern today). The system prompt is explicit that this list is
reference-only and shouldn't be reorganized without a reason from the current
batch. Deleting an initiative resets `initiative_reviewed_at` to NULL for its
former items too — they go back to pending, not into limbo as
already-reviewed orphans.

Both paths write through the same `db::log_*` / `db::assign_*` primitives, so
the UI and schema don't know or care which one ran.

## Polling

One `tokio::spawn` loop (`poller::spawn_cycle_loop`, `COS_POLL_INTERVAL_SECS`,
default 300s) drives both stages every tick — extraction always runs;
classification is gated behind a cheap count so the expensive Opus pass only
fires once there's actually enough to batch:

```mermaid
sequenceDiagram
    participant Loop as poller::run_cycle (every COS_POLL_INTERVAL_SECS)
    participant DB as SQLite (sync_cursors + initiative_reviewed_at)
    participant Src as Slack / GitHub / Linear

    Loop->>DB: get_cursor("slack"), get_cursor("linear")
    Loop->>Src: fetch all, filter to ts/updated_at > cursor
    Loop->>DB: write new items (no initiative_id), advance cursors
    Loop->>DB: count_pending() — 4 cheap COUNT(*) queries, no rows pulled
    alt pending >= COS_BULK_THRESHOLD (default 20)
        Loop->>DB: gather pending + recent standalone items, all 4 tables
        Loop->>DB: one bulk call -> assign_initiative_item for items worth grouping, mark the rest reviewed
    else below threshold
        Loop->>Loop: skip — nothing worth an Opus call yet
    end
```

`poller::run_extraction` and `poller::run_bulk_classification` stay separate,
independently-callable functions — `run_cycle` just sequences them with the
threshold check in between. Three triggers: the startup calls in `main.rs`
(both run unconditionally once, so the UI isn't empty on first boot — the
threshold gate only applies to the recurring loop, not startup), the single
interval loop (`run_cycle`), and — for classification only, bypassing the
threshold entirely — the "Run classification" button, a manual on-demand bulk
pass. `COS_BULK_THRESHOLD` is a count of *pending* items
(`initiative_reviewed_at IS NULL`), not total items — already-reviewed
standalone items don't count toward it, so the gate doesn't fire repeatedly
for a backlog that's already been decided on.

Each source (`slack`, `linear`) has a row in `sync_cursors` holding
`last_synced_at`. A missing row defaults to the epoch, so a brand-new
database's first extraction cycle is a full backfill — "seed" isn't a
separate code path, it's just what the cursor default produces. Every cycle
advances both cursors to the cycle's own timestamp (not the max item
timestamp), and only items newer than the cursor get extracted — this is
what makes the interval loop safe to run against unique-free tables like
`commitments`: without it, every tick would re-insert every historical
commitment again. GitHub status sync isn't cursor-gated — it's "what's the
current state of tracked links," not an event feed, so it always runs in
full each extraction cycle.

Timestamps are UTC with a `Z` suffix everywhere (`util::now()`), matching the
mock fixtures' format, so cursor comparisons are plain string comparison —
no parsing, and no local-timezone-offset bugs from mixing `Local::now()`
output with `Z`-suffixed fixture data (an earlier version of this code had
exactly that bug before the cursor comparisons existed to expose it).

All three sources are real now — `slack.rs`, `github.rs`, `linear.rs`.
`src/mock.rs` has been deleted; nothing in the pipeline is a fixture anymore.
The polling mechanism itself (cursor, interval loop, extraction/
classification dispatch) never knew or cared which sources were live vs.
mocked — that was the point of keeping `insert_linear_tickets` (and the
Slack/GitHub equivalents) as separate, source-agnostic functions from the
start, so swapping each one in was an isolated, low-risk change.

## Environment variables

All four are live — each is actually read, by `agent.rs`, `github.rs`,
`slack.rs`, and `linear.rs` respectively.

| Variable | Used by | Status as of writing |
|---|---|---|
| `ANTHROPIC_API_KEY` | real extraction + initiative classification | live, working |
| `GITHUB_TOKEN` | `github.rs` PR status sync | configured, but repo-scoped access is pending org approval — public repos work (verified against a real `rust-lang/rust` PR), `mercury-eng`-style org repos 403/404 until then |
| `SLACK_USER_TOKEN` | `slack.rs` message/mention/reaction fetch | not set — extraction silently finds 0 new Slack messages every cycle until this exists. Also the one client that's **unverified against a live token** (none exists in this dev environment) — the mention-search query in particular (`<@USER_ID>` as literal query text, no documented `mentions:` modifier exists) should get a real smoke test the moment a token lands. |
| `LINEAR_API_KEY` | `linear.rs` assignedIssues/createdIssues/notifications | live, verified against the real Mercury workspace (86 tickets ingested, 48 bulk-classified into initiatives, 38 correctly left standalone on first run) |

Any of these being absent or erroring is a normal, expected, ongoing state,
not a crash: `poller::run_extraction` degrades per-source (empty result +
a log line) rather than failing the whole cycle, and the startup calls in
`main.rs` log instead of propagating `?`, so the HTTP server always comes up
regardless of which live services are currently reachable.

## Extraction tool calls

Claude gets these as tool definitions during the extraction pass; it calls
whichever apply to a given message/event, or none:

| Tool | Purpose |
|---|---|
| `log_github_link(url, relationship, context, source_permalink)` | merge by `url` — see below |
| `log_commitment(text, due_date, source_permalink)` | insert |
| `log_mention(source_permalink, from_user, text, needs_response)` | upsert by `source_permalink` |
| `log_linear_ticket(identifier, relationship, linked_github_url)` | upsert by `identifier` |
| `resolve(table, id)` | mark done / responded / merged |

Most of these are a thin Rust fn doing `INSERT ... ON CONFLICT DO UPDATE SET
excluded.*` — a second mention of the same thing just overwrites with
whatever the latest call said. `log_github_link` is the one exception,
because blind overwrite was actively wrong there: the LLM classifies each
message in isolation with no memory of what's already tracked, so a PR
jonathan opened (`authored`) could get silently reclassified to `mentioned`
by someone else later casually referencing it, and a `merged` PR could get
reset to `open` because extraction always passes `"open"` as its default
status. So `log_github_link` does an explicit read-then-merge instead of a
raw upsert:

- `relationship` only ever upgrades (`authored` > `reviewing` > `mentioned`
  by rank) — a weaker new signal can't demote a stronger existing one.
- `status` isn't touched by this path at all — it's owned by
  `sync_github_status` (live state) and the manual Resolve action.
- `context` accumulates distinct notes (capped at the last 3, `" | "`
  joined) instead of replacing the previous one.
- `first_seen` / `source_permalink` stay pinned to whichever message first
  reported the PR.

This lives in `db::log_github_link` itself, so both the heuristic and
agent-based extraction paths get it for free — same guarantee either way.

## Web app

Plain CRUD, server-rendered, no SPA framework:

- **axum** router, one module per resource (`github_links`, `commitments`,
  `mentions`, `linear_tickets`), each exposing list/edit/delete handlers
- **tower** middleware: tracing, compression; no auth (localhost-only)
- **htmx** for all list/filter/delete/status-toggle interactions — handlers
  return HTML fragments, not JSON
- **Tailwind** for styling, compiled once at build time (no CDN/JIT at runtime)
- **Preact + signals** only where htmx genuinely isn't enough — e.g. an
  inline due-date editor with client-side validation before submit. Each such
  case is a standalone island: a tiny script mounted into a `<div
  data-island="...">`, not a shared app shell. Default to htmx; reach for an
  island only when state needs to live in the browser between requests.

```mermaid
flowchart TB
    Index["GET /  (dashboard: 4 sections)"] --> GL["GET /github-links (htmx partial)"]
    Index --> CM["GET /commitments (htmx partial)"]
    Index --> MN["GET /mentions (htmx partial)"]
    Index --> LT["GET /linear-tickets (htmx partial)"]
    GL -->|"PATCH /github-links/:url"| Resolve
    CM -->|"PATCH /commitments/:id"| Resolve
    MN -->|"PATCH /mentions/:id"| Resolve
    LT -->|"PATCH /linear-tickets/:id"| Resolve
    Resolve[axum handler] --> DB[(SQLite)]
```

## Project layout (proposed)

```
cos/
  flake.nix
  flake.lock
  nix/
    module.nix           # nixosModules.cos
  Cargo.toml
  src/
    main.rs              # axum app, router assembly
    db.rs                # sqlite pool, schema migrations
    models/
      github_link.rs
      commitment.rs
      mention.rs
      linear_ticket.rs
    routes/
      dashboard.rs
      github_links.rs
      commitments.rs
      mentions.rs
      linear_tickets.rs
    extraction/
      tools.rs           # tool defs + handlers for Claude tool-use
      poller.rs           # Slack/Linear/GitHub poll loop
  templates/              # askama or minijinja templates (htmx fragments)
  static/
    tailwind.css          # built output
    islands/              # preact islands, one file per interactive widget
  tailwind.config.js
  migrations/
```

## Decisions

- **Templating: minijinja**, not askama. Askama embeds templates at compile
  time, so every markup tweak forces a full `cargo build` — a bad loop for an
  agent iterating on fragments. Minijinja loads templates from disk at
  runtime; editing a `.html` file and refreshing the browser is enough.
- **Poll loop**: a `tokio::spawn`ed loop inside the same binary, no separate
  scheduler process.
- **Secrets**: read directly from the process environment
  (`std::env::var`), no `.env`/`dotenvy`. Locally, the CRUD/UI stages don't
  need any secrets at all; only the pollers do, and those are only exercised
  in the deployed NixOS service, where `sops-nix` supplies an
  `EnvironmentFile` to the systemd unit. No secret material lives in the repo
  or the dev shell.
- **Schema: `rusqlite_migration`**, not inline `CREATE TABLE IF NOT EXISTS`.
  The original version ran a big `execute_batch` of `IF NOT EXISTS` DDL on
  every startup — fine for a brand-new file, but silently a no-op on an
  existing one, so adding a column (e.g. `initiative_id`) to an
  already-created table did nothing and broke on the next query. Hit that for
  real mid-project and had to manually delete the dev DB. Now schema changes
  are an ordered `Vec<M>` tracked via SQLite's `user_version` pragma (no
  hand-rolled migrations table) — `db::open` calls `.to_latest(&mut conn)`,
  which applies whatever's pending and is a true no-op otherwise. Future
  schema changes are a new `M::up(...)` appended to the list, never an edit
  to an existing one.

## Nix

```mermaid
flowchart LR
    flake["flake.nix"] --> devShell["devShell\nrust toolchain, sqlite,\ntailwindcss, esbuild, chromium"]
    flake --> pkg["packages.cos\n(crane buildPackage)"]
    flake --> mod["nixosModules.cos"]
    mod --> svc["systemd.services.cos\nEnvironmentFile = sops-nix secret"]
    pkg --> svc
```

- **devShell**: rust toolchain (via rust-overlay/fenix), `sqlite`,
  `tailwindcss`, `nodejs`/`esbuild` (bundling the Preact islands),
  `cargo-watch`, and `chromium` for headless screenshotting. No network
  fetches at test time — everything pinned via the flake.
- **Build: crane**. `craneLib.buildPackage` with a separate `cargoArtifacts`
  derivation so dependency compilation is cached independently of source
  changes — matters once the poller/extraction code pulls in more crates.
  Exposed as `packages.default` / `packages.cos`.
- **NixOS module** (`nixosModules.default`): options for `enable`, `package`
  (defaults to this flake's `packages.cos`), `port`, `dataDir` (SQLite file
  location, via `StateDirectory`), and `environmentFile` (passed straight to
  systemd's `EnvironmentFile=`, pointed at a `sops-nix` secret path in the
  consuming NixOS config). The service runs with `DynamicUser = true`.
- **Headless browser testing**: `chromium --headless=new --screenshot=out.png
  --window-size=1280,800 http://localhost:$PORT` from the devShell is enough
  to visually verify dashboard/fragment rendering — no webdriver server, no
  extra deps. Since the app is server-rendered htmx, functional CRUD checks
  (create/patch/delete) are just `curl`/`httpie` against the routes; the
  browser is only needed to confirm layout and the Preact islands actually
  mount. This is what I'll use to check in with screenshots at each stage
  instead of describing UI changes in prose.

## Suggested build stages

1. `flake.nix` (devShell + crane package skeleton) + axum app scaffold +
   SQLite schema/migrations + static dashboard shell (no data yet) —
   screenshot the empty shell to confirm the toolchain works end to end
2. CRUD routes + minijinja/htmx partials for all four resources, seeded with
   fake data — screenshot each section
3. `nixosModules.cos` + systemd unit sketch (not deployed yet, just
   evaluable: `nix flake check`)
4. Linear poller + extraction tool calls (lowest-friction source, no app
   approval needed)
5. Slack poller once app is approved
6. GitHub status-sync job for tracked links
7. Any Preact islands, only if a CRUD interaction proves awkward in pure htmx
