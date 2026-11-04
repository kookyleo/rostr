# Local quota observations (Phase A)

`rostr quota import PATH` reads one JSON or YAML export, validates it before opening
the database, then records it atomically. It never launches an agent, executes a
credential command, reads authentication files, or connects to a network. The
independent `QuotaProbe` interface exposes only local context and
`AccessPolicy::LocalOnly`; discovery does not run probes.

```sh
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
explicitly imported targets independently of discovery scope; callers must apply
current `scope.yaml` intent separately. Imported targets are user assertions, not
proof that a matching CLI is installed.

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

Export only from a statusline already invoked during normal use. Rostr does not
install or replace statusline hooks and never triggers a prompt to obtain a
sample. The wrapper must preserve the original observation time: copying an old
file does not refresh it. Absent native rate-limit windows supply no balance.
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
Existing discovery tables and scope writes are unchanged. Import (or discovery
save) migrates v1 transactionally; quota query on v1 prints migration guidance.
Database location honors `ROSTR_DATA_DIR` and `XDG_DATA_HOME` like discovery.

Codex native session schema, other native observers, network App Server/ACP and
provider APIs are deferred. No `refresh`, `--network`, background polling,
identity auto-discovery or inference fallback exists in Phase A. Subscription
samples retain observations but do not infer entitlements. Error/backoff DTOs are
available for future probes; there is no control-plane error fetching in this
phase. Per-requirement window selection, custom TTL configuration, automatic
scope intersection and richer billing policies remain follow-up work.
