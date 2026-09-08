# Working in this repo

A read-only MCP server over a self-hosted Actual Budget instance. See `README.md` for what
it does and how to run it; this file covers only what you cannot get from the code.

## Domain rules that silently produce wrong numbers

These caused the only real bugs so far. Each looks like something to simplify away.

- **A transfer to an off-budget account IS spending.** Moving money between two on-budget
  accounts is neutral; moving it to a brokerage or retirement account is money leaving the
  budget, and Actual counts it against whatever category it carries. Excluding all
  transfers understates spending.
- **Category balance is not `budgeted + spent`.** This is an envelope budget: a surplus
  rolls into the next month, so a balance must be accumulated forward from the first month
  with history. Overspending does *not* roll unless the category's carryover flag is set.
- **`accounts.balance_current` is not the balance.** It is whatever the bank last reported
  during a sync. Actual's balance is the sum of transactions, and they routinely disagree.
- **An account balance is not filtered by `TxScope`.** That scope answers "what was spent",
  which excludes transfers and opening balances. A balance counts every movement of money.

## Schema traps

- `v_transactions` filters tombstones. **`v_categories` does not** — write the predicate
  yourself.
- Raw tables and views disagree on column names: `transferred_id`→`transfer_id`,
  `isParent`→`is_parent`, `financial_id`→`imported_id`, `description`→`payee`. CRDT
  messages are applied to **raw tables**; queries read **views**. Never mix the two
  vocabularies in one module.
- `dataset` and `column` on a sync message become SQL *identifiers*, which `?` cannot bind.
  They must be validated against `sqlite_master` and `PRAGMA table_info` before use.
- Take the budget's group id from the `UserFile` listing, never from snapshot metadata —
  a reset-clock upload leaves it null there.

## Where code goes

```
domain.rs        value types only (Money, BudgetDate, BudgetMonth). Never entities.
queries/scope    the single home of every row-inclusion rule
queries/*        a query AND its result struct, together
mcp/tools/*      protocol wrapper only — one file per tool
store/           the local replica; cache, not state
actual/          the only layer that touches the network
```

If two tools would want a type, it belongs in `domain.rs`. If one query produces it, it
lives with that query.

**Errors:** a new *enum* for a new layer, a new *variant* for a new failure mode in an
existing one. Resist a variant per failure site.

## Conventions

- **Functional core, imperative shell.** Pure functions take what they need as parameters —
  `now`, the environment, the candidate list — rather than reaching for globals. This is
  why `is_stale(last, now, ttl)` takes `now` and why config parsing splits into
  `from_env` / `from_vars`.
- **`Err` only when there is nothing useful to return.** An ambiguous name is an `Ok`
  result carrying the candidates; the model can act on that, but not on a failed call.
- **Newtypes enforce representation.** `Money` holds `i64` cents and converts to dollars
  only in its `Serialize` impl. Arithmetic stays in integers — never sum `f64` dollars.
- **Doc comments are the model-facing API documentation.** Every `///` on a tool's `Input`
  and `Output` lands verbatim in the JSON schema the assistant reads. Write them for that
  audience.

## Adding a tool

The tool surface is not documented in prose anywhere, deliberately: `tools/list` is
generated from the code and cannot go stale, whereas a written spec can. What *is* written
down is the standard those descriptions must meet, and it is enforced by tests in
`mcp/server.rs` rather than by convention:

- a description of real substance — a thin one means the model is guessing
- state the units and the sign convention if the tool returns amounts
- say what the figures exclude, in the description and not only in the response
- for parameters taking a name, point at `list_accounts` / `list_categories`
- steer away from misuse, as `query_transactions` does by telling the model to use
  `spending_by_category` for totals rather than summing rows

Registering a tool is three lines outside its own module: a `pub mod`, an import, and one
`.with_async_tool::<T>()`. The test asserting the tool count will fail until you update
it, which is the point.

## Hard constraints

- **stdout is the JSON-RPC channel.** A single `println!` corrupts the stream. All logging
  goes to stderr.
- **Read-only is structural.** Sync requests always carry `messages: Vec::new()`, and no
  endpoint that modifies a budget is called anywhere. Do not add one casually.
- **No personal data in code, comments, or tests.** This repo is public. Fixtures use names
  like `Example Bank Checking (0001)`; comments explain *why* a rule exists in general
  terms, never by quoting a figure from someone's budget.

## Verifying a change

```bash
cargo clippy --all-targets && cargo test
```

- `cargo check` alone **stops at name resolution**, so type errors hide behind unresolved
  names, and it never compiles test code. Always pass `--all-targets`.
- Live tests are `#[ignore]`d and need a real server:
  `set -a; source .env; set +a` — nothing loads `.env` automatically.
- **Check money against the Actual UI, not against plausibility.** A bug once left
  thirteen of fifteen categories correct while the total was wrong.
- Actual rate-limits *failed* logins to 5 per 15 minutes per IP, then rejects valid ones
  for the rest of the window. Do not write tests that spend that budget carelessly.

## Version pins that bite

- `schemars` must match the major version `rmcp` depends on; mixing them yields
  `JsonSchema` errors that point at the wrong line.
- `ToolBase::Parameter` requires `Default`. A tool with no parameters must also override
  `input_schema()` to return `None`.
- Holding a `tokio::sync` lock guard across an `.await` that wants the same lock
  self-deadlocks with no panic and no log. Bind the value to a `let` first; a `match`
  scrutinee holds its temporaries for the whole expression in every edition.
