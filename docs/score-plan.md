# Scoring and selection plan

Status: **plan**. Nothing in this document is implemented yet.

Rostr already discovers models (`rostr discover`, `scope.yaml`, `rostr.db`) and
records local quota observations (`rostr quota refresh|import|query`). The
missing half of the project goal is *evaluate and select*: given a task profile,
rank the models a caller may use and explain why, using metadata and
deterministic algorithms only. This plan defines that layer.

## Goals

- `rostr score` ranks execution targets for a named profile; `rostr select`
  returns the best one. Both are read-only, offline and token-free.
- Every score is reproducible from stored inputs plus `--as-of`, and every rank
  carries machine-readable reasons and provenance.
- Missing evidence stays Unknown. A missing benchmark is never zero, an unknown
  quota is never unlimited, and an unknown capability never satisfies a
  requirement.
- Scores are absolute, not relative: adding or removing a candidate never
  changes another candidate's score (only its rank).

## Non-goals

- Running benchmarks, sending prompts, or calling provider/leaderboard APIs.
  Evaluation data enters only through explicit local imports, like quota exports.
- Routing, proxying or reserving quota. `select` names a target; the caller runs it.
- Learning weights from usage history (possible later, explicitly opt-in).
- Floating point anywhere in the decision path.

## Inputs

| Input | Source | Owner | Storage |
| --- | --- | --- | --- |
| Scope | `scope.yaml` | user intent | file (existing) |
| Known models | discovery | derived | `models`, `model_sources` (existing) |
| Quota eligibility | quota observations | evidence | `current_quota`, `effective_constraints` (existing) |
| Model metadata | `rostr meta import` | evidence | new `model_facts` |
| Evaluation results | `rostr eval import` | evidence | new `eval_sources`, `eval_results` |
| Profiles | `profiles.yaml` | user intent | file (new) |

User intent (scope, profiles) stays in hand-editable config files; evidence
stays in `rostr.db` with provenance. The database never holds a second copy of
intent, matching the existing scope rule.

### Model metadata

Facts keyed by canonical `<provider>/<model>` ID (via `model_id::normalize`):

- `context_window`, `max_output` (tokens, integer)
- `modalities_in`, `modalities_out` (`text`, `image`, `audio`, `pdf`, ...)
- `tool_use`, `structured_output`, `reasoning` (`yes` / `no` / `unknown`)
- `price_in`, `price_out` per million tokens with an explicit currency unit,
  reusing quota's namespaced units and six-digit fixed-point decimals
- `release_date`, `deprecated_at` (optional)

Each fact row stores `source` (`user_export`, `bundled`, `agent_cache`),
`observed_at` and a source event ID, deduplicated like quota imports. Several
sources may disagree; resolution is deterministic: user export > agent cache >
bundled, then newest `observed_at`. Disagreement is reported as a reason, never
averaged.

A small **bundled** fact set (compiled into the binary, versioned, dated) may
cover common models so a fresh install is useful. It is lowest priority and its
age is visible in every explanation. Agent caches already read by discovery
(Codex/Grok `models_cache.json`) can supply context windows when present.

### Evaluation results

```yaml
version: 1
source: {kind: user_export, event_id: swe-bench-2026-10, name: "SWE-bench Verified"}
benchmark:
  id: swe-bench-verified
  version: "2026-09"
  scale: {min: "0", max: "100"}   # declared, required
  higher_is_better: true
results:
  - model: anthropic/claude-sonnet-4-5
    score: "77.2"
    observed_at: 1793754000000
    harness: "vendor-reported"      # opaque label, shown in reasons
```

- `scale` is mandatory. Normalization uses the declared scale, not the
  candidate set, which keeps scores absolute.
- `(benchmark.id, benchmark.version)` is one series. Versions are never mixed;
  a profile names the version it wants or `latest`.
- Same immutability/dedup rules as quota: `(source, event_id)` is unique,
  identical reimports are no-ops, conflicting reuse fails, 1 MiB limit, unknown
  fields rejected.
- Results for models outside the database are stored but flagged; they do not
  create models.

## Profiles

`profiles.yaml` lives next to `scope.yaml` (`$ROSTR_CONFIG_DIR`,
`$XDG_CONFIG_HOME/rostr`, `~/.config/rostr`):

```yaml
version: 1
profiles:
  coding:
    require:
      tool_use: yes
      context_window: {min: 128000}
      quota:
        - {metric: subscription_usage, unit: percent, min_remaining: "20"}
    prefer:                       # weights are integers; only ratios matter
      - {benchmark: swe-bench-verified, version: latest, weight: 6}
      - {benchmark: terminal-bench, version: latest, weight: 3}
      - {factor: quota_headroom, weight: 2}
      - {factor: price, weight: 1, currency: usd}
    unknown: penalize             # penalize | exclude | neutral
    max_age: 120                  # seconds, for quota evidence
```

Validation follows `scope.yaml`: an invalid file stops the command and is never
rewritten. Rostr ships built-in `default` and `coding` profiles that a file may
override by name; built-ins are printed by `rostr score --show-profile NAME`.

## Algorithm

All arithmetic uses the existing `quota::decimal` fixed-point representation
(`i128`, six fractional digits). Steps run in this order and each step records
reasons.

1. **Candidates.** Execution targets from the quota store whose model is
   selected in the current `scope.yaml`, plus selected models with no target
   (they surface as `target: unknown`). This closes the documented gap that
   quota query does not apply scope.
2. **Hard requirements.** Each `require` clause yields Pass / Fail / Unknown.
   Fail removes the candidate (kept in `--explain` output as `blocked`).
   Unknown removes it under `unknown: exclude`; otherwise it stays, marked.
   Quota requirements reuse `QuotaRepository::query_targets` unchanged, so a fresh
   hard pool at zero still blocks.
3. **Factor normalization.** Each `prefer` entry maps to `[0, 1]` in fixed point:
   - benchmark: `(score - min) / (max - min)` on the declared scale, inverted
     when `higher_is_better: false`; clamped, with out-of-scale values flagged.
   - `quota_headroom`: the minimum remaining percent across the target's
     applicable hard percent pools, divided by 100. Non-percent pools do not
     contribute in v1.
   - `price`: `1 - clamp(blended / cap)` where `cap` is a profile parameter
     (default 100 per million); blended = `(3 * in + out) / 4`.
   - `context`: `min(window, cap) / cap`.
4. **Unknown factors.** A missing factor value is handled per profile policy:
   - `penalize` (default): contributes 0 *and* sets `coverage < 1`.
   - `neutral`: dropped, remaining weights renormalized.
   - `exclude`: candidate removed.
   Coverage (`known weight / total weight`) is always reported, so a high score
   from one benchmark is visibly different from a high score from five.
5. **Aggregate.** `score = Σ(weight × value) / Σ(weight)`, rounded half-even to
   six digits. Reported both as the fixed-point decimal and as `0–1000` integer
   points for display.
6. **Order.** Score desc, then coverage desc, then eligibility
   (`eligible` > `unknown`), then authoritative quota confidence, then canonical
   target ID asc. The final key makes the order total and stable.

`select` is `score` with `--limit 1` that exits non-zero when no candidate
passes, so agents can branch on the status code.

## CLI

```sh
rostr meta import PATH                       # model facts (JSON/YAML)
rostr eval import PATH                       # benchmark results (JSON/YAML)
rostr eval list [--benchmark ID] [--json]    # stored series and coverage
rostr score --profile coding [--agent A] [--model M]... \
  [--as-of MS] [--limit N] [--explain] [--json]
rostr select --profile coding [--json]
```

JSON output per candidate: `target`, `model`, `score`, `points`, `coverage`,
`eligibility`, `factors[]` (`name`, `raw`, `normalized`, `weight`, `source`,
`observed_at`), `requirements[]` (`clause`, `result`, `reason`), and `rank`.
The top-level object includes the resolved profile and its hash so results can
be compared across runs.

`score` opens the database read-only in one transaction, like `quota query`,
and never migrates or refreshes. Imports migrate.

## Storage (schema v3)

```sql
CREATE TABLE fact_sources (id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
  event_id TEXT NOT NULL, imported_at INTEGER NOT NULL, UNIQUE(kind, event_id));
CREATE TABLE model_facts (source_id INTEGER NOT NULL REFERENCES fact_sources(id),
  model_id TEXT NOT NULL, fact TEXT NOT NULL, value TEXT NOT NULL,
  observed_at INTEGER NOT NULL, PRIMARY KEY(source_id, model_id, fact));
CREATE TABLE benchmarks (id TEXT NOT NULL, version TEXT NOT NULL,
  scale_min TEXT NOT NULL, scale_max TEXT NOT NULL, higher_is_better INTEGER NOT NULL,
  PRIMARY KEY(id, version));
CREATE TABLE eval_sources (id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
  event_id TEXT NOT NULL, name TEXT, imported_at INTEGER NOT NULL, UNIQUE(kind, event_id));
CREATE TABLE eval_results (source_id INTEGER NOT NULL REFERENCES eval_sources(id),
  benchmark_id TEXT NOT NULL, benchmark_version TEXT NOT NULL, model_id TEXT NOT NULL,
  score TEXT NOT NULL, harness TEXT, observed_at INTEGER NOT NULL,
  PRIMARY KEY(source_id, benchmark_id, benchmark_version, model_id),
  FOREIGN KEY(benchmark_id, benchmark_version) REFERENCES benchmarks(id, version));
CREATE VIEW current_eval AS ...  -- newest observed_at per (benchmark, version, model)
```

`model_id` columns are plain text, not foreign keys, so deleting a model in
discovery never cascades into evidence (same rule as quota history). The v2→v3
migration is additive and transactional; existing tables are untouched.

## Module layout

- `src/facts/` — DTOs, import, bundled set, resolution.
- `src/eval/` — DTOs, import, `current_eval` access.
- `src/score/` — `profile.rs` (load/validate), `engine.rs` (pure function:
  `score(inputs, profile, as_of) -> Vec<Ranked>`), `cli.rs`.

The engine takes plain structs, not a database handle, so it is unit-testable
without SQLite and trivially deterministic.

## Milestones

1. **Profiles + engine core.** Profile parsing/validation, fixed-point
   normalization and aggregation, ordering, Unknown policies. Pure unit tests,
   including property tests that a candidate's score is independent of the
   other candidates and that order is total.
2. **Evaluation import.** Schema v3, `eval import`, `eval list`, fixtures,
   dedup/conflict tests mirroring `tests/quota.rs`.
3. **`rostr score` / `select`.** Scope intersection, quota requirement reuse,
   `--explain`, JSON contract, exit codes; integration tests with temp dirs.
4. **Model metadata.** `meta import`, source precedence, capability
   requirements, context/price factors; agent cache facts where discovery
   already reads them.
5. **Bundled facts.** Optional, dated, lowest-priority data set; separate
   release cadence decided before shipping.
6. **Docs.** README status/usage, `docs/score.md` replacing this plan.

Each milestone keeps `cargo fmt --check`, `cargo check` and `cargo test` green
and ships independently.

## Open questions

- Should `quota_headroom` consider non-percent pools (tokens, credits) once a
  limit is known, or stay percent-only?
- Is `penalize` the right default, or should the default be `exclude` for
  benchmarks named with a high weight?
- Bundled facts: ship them at all, or keep Rostr evidence-only and document a
  community export repo instead?
- Should profiles support per-agent preference (e.g. prefer subscription routes
  over API keys) as a factor, or is `--billing subscription` enough?
- Do agents need a stable "why not X" query (`rostr score --explain --model X`)
  beyond what `--explain` already prints?
