# Local quota observations

`rostr quota refresh --local` reads existing Codex and Grok Build state directly,
plus the Claude Code status line snapshot that `rostr quota init --claude` sets up.
`rostr quota import PATH` remains available for explicit JSON/YAML exports. Both
validate observations before recording each report atomically. It never launches an agent, executes a
credential command, reads authentication files, or connects to a network. The
independent `QuotaProbe` interface exposes only local context and
`AccessPolicy::LocalOnly`; discovery does not run probes.

```sh
rostr quota init --claude          # once: wrap the Claude Code statusLine
rostr quota refresh --local
rostr quota refresh --local --agent codex --json
rostr quota import tests/fixtures/quota/claude-statusline.json
rostr quota query --agent claude-code --metric subscription_usage --unit percent \
  --min-remaining 20 --max-age 120 --json
# Replay freshness checks against the fixture timestamp:
rostr quota query --metric subscription_usage --unit percent --min-remaining 20 \
  --as-of 1793754001000 --json
```

`--max-age` is in seconds; exported timestamps and `--as-of` are UTC Unix
milliseconds. Native Claude `resets_at` is Unix seconds and converted explicitly.
`--as-of` evaluates current evidence at a supplied time; it is not historical SQL
replay. Newer observations may already have superseded older history.

Query opens an existing database read-only, in one read transaction, and never
migrates or refreshes it. No database returns an empty list (no known execution
targets), not an unlimited quota. `--confidence authoritative` rejects user
exports for eligibility; `--unknown exclude` filters Unknown targets;
`--billing subscription` requires an explicitly declared subscription route.
Defaults include Unknown, allow all evidence classes and billing modes.

The JSON result contains identity, all current applicable pools, per-pool source
provenance, eligibility, reasons and query time. Any fresh hard pool at zero
blocks the target, even when another window meets the requested threshold.
Missing, stale, unknown-enforcement, anomalous or unit-mismatched evidence yields
Unknown. Unlimited must be explicit. Advisory pools cannot prove eligibility.
Thresholds use exact fixed-point comparisons (six fractional decimal digits),
including currency/credit namespaces in unit identity; the CLI exposes common
scalar units, and the Rust API also supports namespaced units.

Eligible means only that the supplied observations meet the declared constraints.
It does not authenticate a route, verify entitlement or reserve quota. Query lists
observed targets independently of discovery scope; callers must apply
current `scope.yaml` intent separately. Imported targets are user assertions. Native targets describe local cache evidence,
which does not prove authentication or current execution routing.

## Native Codex and Grok Build observers

The independent native registry runs both file readers on local refresh and reports
`not_installed`, `no_observation`, `unsupported`, or `partial` per probe. No agent
is launched, no App Server/ACP/HTTP call is made, and auth files are never opened.
Directory existence or an executable found on PATH only establishes installation.
`--agent claude-code|codex|grok-build` restricts refresh; `--max-age` defaults to
60 seconds. Refresh also reads the fixed Claude status line snapshots described
below. An optional `--claude-statusline PATH` reads any other file in the same
export format. Rostr never guesses a Claude cache path: Claude Code keeps no
quota on disk, so the snapshot exists only after `rostr quota init --claude`.

| Observer | Files read | Accepted evidence |
| --- | --- | --- |
| Codex | `$CODEX_HOME/sessions/**/*.jsonl`, default `~/.codex/sessions` | `session_meta.model_provider`, `turn_context.model`, and `event_msg` / `token_count` / `rate_limits` |
| Grok Build | `$GROK_HOME/logs/unified.jsonl` and bounded `config.toml`, default `~/.grok` | Exact shell/info `billing: fetched credits config` event with `ctx.config.creditUsagePercent` and `currentPeriod` |

Codex uses the core rollout snake_case schema: independent `limit_id` snapshots,
`primary` and `secondary` windows with numeric `used_percent`, positive integer
`window_minutes`, and Unix-second `resets_at`. Missing/null `limit_id` uses the
legacy `codex` pool name. Only OpenAI-provider rollouts with model evidence are
supported. CamelCase App Server payloads are not interpreted as rollouts. All
recognized limit IDs remain separate; this parser does not collapse them into a
single percentage. Fixtures pin this subset to the
[Codex protocol source](https://github.com/openai/codex/blob/53ff712a48379ce8df605e292afd6046ca88ae9b/codex-rs/protocol/src/protocol.rs).

Grok accepts RFC3339 `ts`, weekly/monthly `currentPeriod.type`, and RFC3339
`start`/`end`; it preserves the period end as reset. The supported event is emitted
by the [billing implementation](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-shell/src/extensions/billing.rs),
using the [unified log writer](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-telemetry/src/logs/unified_log.rs).
The configured model labels only describe candidate targets. No configured model
creates a balance. With no configured model, `xai/unknown` explicitly represents
unresolved model evidence. Session `signals.json` token totals, monetary legacy
monthly usage, prepaid balance, and on-demand caps are not interpreted as included
period quota. Exhausting the included pool cannot prove that on-demand access is
unavailable.

These schemas supply no verified account identity. Codex isolates unresolved
accounts per rollout path; Grok isolates them per billing event time and config
root. Labels and event IDs are SHA-256 digests of local scope or extracted
observations; paths, prompts, auth values, and raw logs are not stored. Multiple
Grok model candidates bind to one pool/sample, rather than multiplying its balance.
Different rollouts are not assumed to share an account. Native buckets have
`identity_unresolved`, Unknown confidence/enforcement, and reports are Partial;
queries therefore remain Unknown even when a numeric percentage is available.
An explicit zero is preserved as zero; neither missing data nor schema drift
becomes zero or Unlimited. Native data is not promoted to an authenticated account
by whatever credentials happen to exist now.

Original event time is preserved, with TTL capped by reset. Repeated identical
Codex windows within the scanned evidence retain the earlier timestamp because
Codex can re-emit cached limits alongside newer token usage. Even the first seen
event may contain cached provider data: this is bounded historical evidence, not
a live quota request. Copying files or refreshing Rostr does not renew timestamps.
No absolute token limits are inferred from percentages, model context, plans, or
session usage. Out-of-range percentages retain their original value and have
Unknown remaining balance.

Reads are bounded: up to 128 Codex files, five directory levels, 4,096 directory
entries total (at most 512 per directory), a 256 KiB prefix and roughly 4 MiB tail
per file, and 256 KiB per JSONL record. Enumeration prefers descending names within
the bounded directory sample; it cannot guarantee the newest file in larger
directories. Incomplete final lines and unrelated records are ignored. Oversized
records or unsupported recognized schemas do not fabricate samples. Symlink files
and discovered symlink subdirectories are skipped; explicitly configured roots
may themselves be symlinks. Successful bounded reads remain Partial. Unsupported
versions, missing windows, history outside these bounds, account binding, model
specific sharing, and richer credit/billing formats remain gaps. No observer
executes configuration commands or tries a network fallback.

## Claude Code status line (`rostr quota init --claude`)

Claude Code has no silent `/usage` call and no on-disk quota cache. Its official
channel is `statusLine.command`: Claude Code runs one command and feeds it the
session JSON on stdin. For Claude subscribers (Pro/Max and similar), that JSON
carries `rate_limits.five_hour` and `rate_limits.seven_day` after the first API
response of a session. Rostr hooks into this channel. It is not a proxy: it never
sees Anthropic traffic, starts Claude, reads credentials or uses a network.

```sh
rostr quota init --claude                    # wrap or install; safe to re-run
rostr quota init --claude --dry-run          # show what would change
rostr quota init --claude --account work     # set the opaque account label
rostr quota init --claude --uninstall        # restore the previous status line
```

`init` edits `settings.json` in `$CLAUDE_CONFIG_DIR`, else `~/.claude` (override
with `--claude-config-dir DIR`):

- **A status line exists:** Rostr wraps it. The `statusLine` object keeps its other
  fields (such as `padding`); only `command` becomes
  `<rostr> quota statusline --state <state file>`. The previous object is saved in
  the state file.
- **No status line:** Rostr installs the same wrapper, which prints a short line
  such as `Sonnet · 5h 25% · 7d 50%`.
- **Re-running** detects the Rostr command (` quota statusline --state `) and
  updates it in place, so wrappers never stack. The previous command and account
  label are kept unless `--account` is given. If you replace the status line
  yourself after installing, the next `init` wraps the new command.
- Other keys and their order are preserved. The file is replaced atomically, with
  its permissions; a symlinked `settings.json` is written through its target. If
  the file changes while Rostr edits it, nothing is written. The first `init`
  copies the original file to the settings backup below.

On every status line update the wrapper:

1. reads stdin once (up to 8 MiB);
2. if the JSON has a `five_hour` or `seven_day` window with a numeric
   `used_percentage`, writes a version 1 `claude_statusline` envelope (the format
   below) atomically to the fixed snapshot path. Only `used_percentage` and
   integer `resets_at` of those two windows are kept. Session ID, paths, cost,
   context window and every other field are dropped. Without rate limits (API key
   users, or before the first response) the last snapshot is left unchanged;
3. re-feeds the same bytes to the previous command through `sh -c` (`cmd /C` on
   Windows), passing its stdout, stderr and exit status through. Without a previous
   command it prints the minimal line.

A Rostr error never breaks the status bar: the wrapper still prints a line and
exits 0. `ROSTR_STATUSLINE_WRAPPED` guards against a wrapper calling itself.

| File | Path |
| --- | --- |
| state | `<config dir>/claude-statusline/<instance>.json` (account, paths, previous `statusLine`) |
| settings backup | `<config dir>/claude-statusline/<instance>.settings-backup.json` |
| snapshot | `<data dir>/claude-statusline/<instance>.json`, read by `refresh --local` |

`<config dir>` and `<data dir>` are the scope and database directories (see the
README). The wrapper command contains the absolute paths resolved at `init` time,
so it works even when Claude Code runs without your shell variables. Re-run
`init` after moving the `rostr` binary. The instance is `default` for `~/.claude`,
else `dir-<digest of the directory>`, or `--instance NAME`.

`refresh --local` reads up to 16 snapshot files, one report per instance with
probe ID `claude-statusline`. No file gives `not_installed` (no Claude config
directory) or `no_observation`; a damaged file gives `unsupported` without
stopping other observers. `rostr quota import <snapshot>` also still works.

**Identity.** Auto-filled: `agent: claude-code`, `instance`, `model` (from
`model.id`, canonicalized like discovery, else `anthropic/unknown`), `raw_model`,
`raw_provider: anthropic`, `route: anthropic-subscription`, `billing_mode:
subscription` (Claude only reports these windows to subscribers), and
`observed_at` (the wrapper's clock: status line JSON has no timestamp, and the
values are as fresh as Claude's last API response). User-supplied: `account`, an
opaque label that defaults to `claude-<instance>`. Rostr does not read the logged-in
account; use a different `--account` (or Claude config directory and instance) per
Claude login so that two subscriptions never share a balance. `profile` is null.
Buckets are `user_supplied` confidence, valid for at most 60 seconds (or
`--max-age`) after observation and never past their reset.

**Policy checks.** `init` reads Claude Code managed settings
(`/etc/claude-code/managed-settings.json`, `/Library/Application
Support/ClaudeCode/managed-settings.json`, `C:\Program
Files\ClaudeCode\managed-settings.json`, and `managed-settings.d/*.json` beside
them) and stops without writing when:

- managed `disableAllHooks` is true (it also disables status lines);
- managed settings define `statusLine` (it overrides user settings; ask the
  administrator to wrap the managed command);
- managed `allowManagedHooksOnly` is true, unless `--force` is given;
- your own `disableAllHooks` is true (Rostr never flips it);
- `settings.json` is not valid JSON, or `statusLine` is not a `command` object.

An unreadable managed file is a warning. A project `.claude/settings.json` or
`settings.local.json` with its own `statusLine` overrides the user setting in that
project, so no snapshot is written there.

**Uninstall.** `rostr quota init --claude --uninstall` puts the saved previous
`statusLine` object back exactly, or removes `statusLine` if Rostr installed the
only one, and deletes the state file. If the current status line is not Rostr's,
settings are left alone. The snapshot, the backup and recorded history are kept;
delete them by hand if you like. To restore by hand, copy `previous` from the state
file into `statusLine`, or restore the settings backup.

## Claude statusline export

The version 1 Rostr envelope wraps an existing native statusline payload with an
opaque account clue, execution identity and original observation time. See the
[isolated fixture](../tests/fixtures/quota/claude-statusline.json). The parser
supports the documented `rate_limits.five_hour` and `seven_day` subset, each with
`used_percentage` and optional Unix-second `resets_at`. Other statusline fields
are ignored and never stored. Unknown rate-limit fields fail validation instead
of being silently interpreted. The source format is pinned by this fixture and
the [statusline documentation](https://code.claude.com/docs/en/statusline)
reviewed in the design notes on 2026-11-04. New gateway monetary window formats
are not supported yet.

Export only from a statusline already invoked during normal use. Rostr changes
the status line only through the explicit `rostr quota init --claude` above, and
never triggers a prompt to obtain a sample. A wrapper must preserve the original
observation time: copying an old file does not refresh it. Absent native rate-limit windows supply no balance.
Context-window usage and session token counts are ignored. Percent remains percent
and is never converted into a token budget. Values outside 0–100 are preserved with
an anomaly flag and unknown remaining balance.

## Generic JSON/YAML export

Use `{ "version": 1, "report": <ProbeReport> }` instead of `claude_statusline`.
The two envelope forms are mutually exclusive. This allows other agents to supply
observations without pretending their local session usage is remaining quota.
All DTOs are exposed through `rostr::quota`; a YAML round-trip is covered by the
integration tests. A compact generic example:

```yaml
version: 1
report:
  probe_id: manual-export
  version: 1
  started_at: 1793754000000
  collected_at: 1793754000000
  source: {kind: user_export, schema_version: 1, event_id: example-1}
  completeness: complete
  bindings:
    - target:
        agent: codex
        instance: default
        model: openai/gpt-5.3-codex
        route: subscription
        account: account-example
        raw_provider: openai
        raw_model: gpt-5.3-codex
        profile: null
        billing_mode: subscription
      pool_keys: [primary]
  buckets:
    - pool_key: primary
      account: account-example
      metric: subscription_usage
      unit: percent
      window_key: five_hour
      window_kind: rolling
      duration_ms: 18000000
      state: known
      used: "65"
      remaining: "35"
      hard_limit: null
      enforcement: hard
      observed_at: 1793754000000
      valid_until: 1793754060000
      reset_at: 1793761200000
      confidence: user_supplied
```

Only opaque non-secret labels are allowed for identities. Never export keys,
bearers, cookies, email addresses, credential URLs or prompts. There is no need
to inspect a credential to create a binding. Account labels are user assigned:
reuse one only when the shared billing identity is established. Never merge by
model provider or key prefix. Instance, model, raw route/provider, account,
profile and billing mode participate in a deterministic, collision-free identity.
An account switch creates a different target and cannot inherit a prior balance.

Every supplied pool requires an explicit same-account target binding. Binding
`pool_keys` declares applicable pools; a reference with no observation remains
unknown. Omitted previously observed windows are retracted on a newer complete
or partial snapshot from that probe/account. Failed/no-observation reports retain
successful history without publishing samples. Snapshot exports should therefore
represent the complete set of currently observed windows for each account/source.

Import downgrades bucket confidence to UserSupplied, rejects unknown envelope/DTO
fields, limits file size to 1 MiB and suppresses parser excerpts. It overwrites
collection time with the local clock, preserving observation time; it caps bucket
freshness at 60 seconds and at reset/explicit expiry. Missing remaining stays null;
no implicit arithmetic is done for generic reports. Negative, nonfinite or
excessively precise generic quantities are rejected, except anomalous signed
percentage usage, which is preserved with a quality flag. Use `state: unknown`,
`unlimited` or `not_applicable` explicitly; no sentinel amounts.

The pair `(probe_id, source.event_id)` is immutable and deduplicated. Identical
reimports (apart from collection time) are no-ops; conflicting reuse fails.
Use a new event ID for a new snapshot. A report import is one transaction, so
failure never leaves a partial successful sample. Full raw statusline payloads,
credentials and free-form imported diagnostics are not persisted.

## Storage and gaps

Schema v2 adds agent instances, accounts, execution targets, pools/bindings,
subscription/quota/rate-limit/availability samples, sources, probe runs and account
snapshot boundaries. `current_quota` and `effective_constraints` expose latest
samples with nullable decimal TEXT amounts; never compare their text
lexicographically. Shared pools are stored once regardless of target count.
History remains intact, and model deletion cannot cascade into quota history.
Existing discovery tables and scope writes are unchanged. Refresh, import, or discovery
save migrates v1 transactionally; quota query on v1 prints migration guidance.
Database location honors `ROSTR_DATA_DIR` and `XDG_DATA_HOME` like discovery.

OpenCode/Gemini/Pi native observers, network App Server/ACP and provider APIs remain
deferred. There is no `--network`, background polling, authenticated identity
auto-discovery or inference fallback. Subscription samples retain observations but
do not infer entitlements. Error/backoff DTOs remain available for future probes.
Per-requirement window selection, custom TTL policy, automatic scope intersection
and richer billing policies remain follow-up work.
