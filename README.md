# Actual MCP

A [Model Context Protocol](https://modelcontextprotocol.io) server that gives an AI
assistant **read-only** access to a self-hosted [Actual Budget](https://actualbudget.org)
(GitHub) instance.

Ask Claude *"how much did I spend on groceries last month, and how does that compare to
what I budgeted?"* and it answers from your real budget — without being able to change
a single transaction.

## Why this exists

Actual's sync server is a storage and sync layer, not a query API. It holds your budget
as a compressed SQLite snapshot plus an append-only log of CRDT edits, and there is no
endpoint that will answer a question about either. Every Actual client — the web UI, the
desktop app, this server — rebuilds the budget locally before it can read it.

This server does that rebuild in Rust, keeps a local replica current, and exposes a small
set of tools over MCP. It is **read-only by construction**: the only write path in the
codebase writes to a disposable cache on your own machine.

- No endpoint that modifies your budget is ever called.
- Sync requests always carry an empty message list, so nothing is pushed back.
- Deleting the cache directory is complete, safe recovery.

## Architecture

```
   ┌──────────────┐
   │    BANKS     │
   └──────┬───────┘
          │  ❶ bank import (SimpleFIN / GoCardless)
          │     you trigger this in Actual; nothing here does it for you
          ▼
   ┌─────────────────────────────────────────────┐        ┌──────────────────┐
   │        ACTUAL SYNC SERVER  :5000            │◀──────▶│    ACTUAL UI     │
   │                                              │        │  browser / app   │
   │   file-<id>.blob      zip: db.sqlite + meta  │  push  │  read + write    │
   │   group-<id>.sqlite   append-only CRDT log   │  pull  └──────────────────┘
   │                                              │
   │   the source of truth — but not queryable    │
   └────────────────────┬────────────────────────┘
                        │
                        │  ❷ read-only pull: snapshot + messages since our cursor
                        │     GET  /sync/download-user-file
                        │     POST /sync/sync   { since, messages: [] }
                        ▼
   ┌─────────────────────────────────────────────┐
   │         actual-mcp  (this project)          │
   │                                              │
   │   local replica in $XDG_CACHE_HOME           │
   │   snapshot ⊕ replay(messages) = current      │
   │   cache, not state — safe to delete          │
   └────────────────────┬────────────────────────┘
                        │
                        │  ❸ MCP over stdio (JSON-RPC 2.0)
                        ▼
   ┌─────────────────────────────────────────────┐
   │   MCP CLIENT — Claude Desktop / Claude Code  │
   └─────────────────────────────────────────────┘
```

Two different things are called "sync", and only one of them is ours:

| | What moves | Who does it |
|---|---|---|
| ❶ **Bank import** | bank → Actual | **You**, in the Actual UI |
| ❷ **State retrieval** | Actual → local replica | This server, automatically |
| ❸ **MCP** | replica → assistant | This server, per tool call |

Edits you make in the Actual UI are pushed to the server as you work and appear here on
the next refresh. Nothing ever travels the other way.

## Tools

| Tool | Answers |
|---|---|
| `list_accounts` | balances, on/off-budget, open/closed, net worth |
| `list_categories` | categories and their groups, in budget order |
| `spending_by_category` | per-category totals over a date range |
| `get_monthly_budget` | budgeted / spent / remaining for a month |
| `query_transactions` | individual transactions, filtered by date, account, category, payee, amount or text |
| `get_data_freshness` | how current the data is, and when transactions were last imported |

Amounts are returned in dollars, negative for outflows. Accounts, categories and payees
are addressed **by name** — a name matching several things comes back as a list of
candidates rather than a guess.

Spending figures exclude transfers between your own on-budget accounts, activity inside
off-budget accounts, closed accounts, and opening balances. Money moved from an on-budget
account *into* an off-budget one **is** counted, because it leaves the budget.

## Requirements

- Rust 1.85 or newer (the crate uses edition 2024)
- A running Actual sync server you can reach over HTTP
- A budget that is **not** end-to-end encrypted (unsupported; the server refuses it with a
  clear error rather than returning nonsense)

## Install

**1. Build**

```bash
git clone https://github.com/jwke21/actual-mcp.git
cd actual-mcp
cargo build --release
```

The binary lands at `target/release/actual-mcp`. Note its absolute path — the MCP
client needs it.

**2. Find your Sync ID (only if you have more than one budget)**

In Actual: **Settings → Advanced → Sync ID**. With a single budget you can skip this; the
server selects it automatically.

**3. Configure**

The server is configured entirely through environment variables.

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `ACTUAL_SERVER_URL` | yes | — | e.g. `http://localhost:5006` |
| `ACTUAL_PASSWORD` | yes | — | your Actual server password |
| `ACTUAL_SYNC_ID` | no | sole budget | which budget, when there are several |
| `ACTUAL_CACHE_DIR` | no | `$XDG_CACHE_HOME/actual-mcp` | where the replica lives |
| `ACTUAL_TTL_SECONDS` | no | `60` | how often to pull from Actual |

**4. Check it runs**

```bash
ACTUAL_SERVER_URL=http://localhost:5006 ACTUAL_PASSWORD='your-password' \
  ./target/release/actual-mcp
```

On the first run it downloads a snapshot and builds the replica; afterwards it reuses the
cache. It then waits silently for JSON-RPC on stdin — that is correct. Press Ctrl-D to
exit. Any problem is reported on stderr.

> **A note on wrong passwords.** Actual rate-limits *failed* logins to 5 per 15 minutes
> per IP. Once tripped it rejects every login for the rest of the window, correct ones
> included, with `too-many-requests`. If you see that, wait it out or restart the Actual
> container.

## Use with Claude Code

From anywhere:

```bash
claude mcp add actual-budget \
  -e ACTUAL_SERVER_URL=http://localhost:5006 \
  -e ACTUAL_PASSWORD='your-password' \
  -- /absolute/path/to/actual-mcp
```

Then check it registered:

```bash
claude mcp list
```

Start a session and ask something. `/mcp` lists the connected servers and their tools.

## Use with Claude Desktop

Edit the config file:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "actual-budget": {
      "command": "/absolute/path/to/actual-mcp",
      "env": {
        "ACTUAL_SERVER_URL": "http://localhost:5006",
        "ACTUAL_PASSWORD": "your-password"
      }
    }
  }
}
```

Restart Claude Desktop. The tools appear under the connector icon in the message
composer.

### Windows with the server built in WSL

Windows cannot execute a Linux binary directly, so invoke it through `wsl.exe`. One extra
step is required: **`wsl.exe` does not pass Windows environment variables into Linux
unless `WSLENV` names them.** Without it the server starts and immediately exits with
`ACTUAL_SERVER_URL is not set`, and the client reports only "Server disconnected".

```json
{
  "mcpServers": {
    "actual-budget": {
      "command": "wsl.exe",
      "args": ["-e", "/home/you/actual-mcp/target/release/actual-mcp"],
      "env": {
        "ACTUAL_SERVER_URL": "http://localhost:5000",
        "ACTUAL_PASSWORD": "your-password",
        "WSLENV": "ACTUAL_SERVER_URL:ACTUAL_PASSWORD"
      }
    }
  }
}
```

`WSLENV` is a colon-separated list of variable names to carry across the boundary. Any
variable missing from it is dropped without warning, so add each optional setting you use
(`ACTUAL_SYNC_ID`, `ACTUAL_CACHE_DIR`, `ACTUAL_TTL_SECONDS`) to the list as well.

If your Actual server also runs inside WSL, `localhost` resolves correctly from there.

## Verifying it works

Ask your assistant:

> How much did I spend on groceries last month, and how does that compare to what I
> budgeted?

A good answer names a figure, compares it to the budgeted amount, and — if your last bank
import was a while ago — says so. Cross-check the number against Actual's own budget
screen for the same month; they should agree exactly.

## Keeping data current

This server pulls from Actual automatically, but Actual only gains new transactions when
**you** run a bank import. `get_data_freshness` reports both clocks separately so the
assistant can tell you when it is answering from stale data rather than asserting it as
current.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `ACTUAL_SERVER_URL is not set` | environment variables did not reach the process. In Claude Desktop they must be in the `env` block, not your shell — and under WSL they must also be named in `WSLENV` |
| `authentication failed: invalid-password` | wrong password |
| `Actual server returned 429: too-many-requests` | login rate limiter; see the note above |
| `budget '<name>' is end-to-end encrypted` | unsupported |
| `multiple budgets found; set 'ACTUAL_SYNC_ID'` | pick one with the Sync ID from Settings → Advanced |
| Numbers look stale | run a bank import in Actual, then ask again |

Deleting the cache directory forces a clean rebuild and is always safe.

## Development

```bash
cargo test                  # unit tests, offline
cargo clippy --all-targets
```

Integration tests run against a live server and are `#[ignore]`d by default:

```bash
ACTUAL_SERVER_URL=http://localhost:5006 ACTUAL_PASSWORD='...' \
  cargo test --test actual -- --ignored --nocapture
```

They assert structurally by default. To pin them to a known instance, set
`ACTUAL_TEST_BUDGET_NAME`, `ACTUAL_TEST_FILE_ID`, `ACTUAL_TEST_GROUP_ID` or
`ACTUAL_TEST_TXN_COUNT`.

## Scope

Deliberately not implemented: writes of any kind, triggering bank imports, multiple
budgets in one session, end-to-end encrypted files, MCP resources and prompts, and HTTP
transport.

## License

MIT
