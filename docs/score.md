# Public benchmark scores

`rostr score` attaches public benchmark results to canonical model IDs on five
axes: `general`, `coding`, `reasoning`, `multimodal` and `instruction`.

```sh
rostr score refresh                     # the only networked command
rostr score sources                     # sources, licenses, URLs, rank table, last snapshots
rostr score query                       # selected models from `rostr discover`
rostr score query --model anthropic/claude-opus-4-5 --axis coding --json
rostr score query --all --max-age 7     # every known model; Stale after 7 days
rostr score import --source epoch-ai ./benchmark_data.zip    # offline snapshot
rostr score import --source lmarena  ~/.local/share/rostr/scores/lmarena
```

## Network boundary

Only `rostr score refresh` opens network connections. It requests only the
fixed HTTPS URL prefixes the sources declare (see `rostr score sources`),
verifies TLS with the platform trust store (`SSL_CERT_FILE` is honored), follows
at most three HTTPS redirects, uses the `HTTPS_PROXY` environment proxy, and
caps every response (64 MiB for the Epoch zip, 4 MiB per LMArena page and at
most 400 pages per config, 32 MiB per unzipped CSV). No credentials or cookies are sent.

Raw files are cached byte-for-byte under `<data dir>/scores/<source>/` and
replaced only after the new snapshot parsed. `score import` parses the same
formats from a local file or directory without any network access.

`score query` and `score sources` open `rostr.db` read-only. They never refresh,
migrate or write. With no snapshot, every axis is Unknown.

## Sources

| Source ID | Data | License | URL |
| --- | --- | --- | --- |
| `epoch-ai` | Epoch AI *Capabilities & benchmarking* | CC BY 4.0 | `https://epoch.ai/data/benchmark_data.zip` |
| `lmarena` | LMArena `lmarena-ai/leaderboard-dataset`, split `latest` | CC BY 4.0 | `https://datasets-server.huggingface.co/rows?dataset=lmarena-ai%2Fleaderboard-dataset&...` (configs `text`, `vision`) |

Attribution: *Epoch AI, ‘Capabilities & benchmarking’, https://epoch.ai/benchmarks*;
*LMArena, lmarena-ai/leaderboard-dataset, https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset*.

Metrics read from them (values stay on the board's native scale):

| Metric | Source file / board | Column | Unit |
| --- | --- | --- | --- |
| `epoch-ai/eci` | `epoch_capabilities_index/eci_scores.csv` | `eci` | index |
| `epoch-ai/swe-bench-verified` | `swe_bench_verified.csv` (Epoch run) | `Best score (across scorers)` | fraction |
| `epoch-ai/aider-polyglot` | `aider_polyglot_external.csv` | `Percent correct` | percent |
| `epoch-ai/gpqa-diamond` | `gpqa_diamond.csv` (Epoch run) | `Best score (across scorers)` | fraction |
| `epoch-ai/hle` | `hle_external.csv` | `Accuracy` | fraction |
| `epoch-ai/video-mme` | `video_mme_external.csv` | `Overall (no subtitles)` | fraction |
| `lmarena/text-overall` | config `text`, category `overall` | `rating` | elo |
| `lmarena/text-coding` | config `text`, category `coding` | `rating` | elo |
| `lmarena/text-instruction-following` | config `text`, category `instruction_following` | `rating` | elo |
| `lmarena/vision-overall` | config `vision`, category `overall` | `rating` | elo |

`as_of` is the board's own date when it has one: Epoch's `Started at` or
`Date of evaluation`, and LMArena's `leaderboard_publish_date`. It is null for
ECI, HLE and Video-MME, which carry only model release dates.

## Axis rank table

For each model and axis, the first qualified observation in this order wins.
Values from different sources or metrics are never averaged or combined.

| Axis | Order |
| --- | --- |
| general | `epoch-ai/eci` > `lmarena/text-overall` |
| coding | `epoch-ai/swe-bench-verified` > `epoch-ai/aider-polyglot` > `lmarena/text-coding` |
| reasoning | `epoch-ai/gpqa-diamond` > `epoch-ai/hle` |
| multimodal | `lmarena/vision-overall` > `epoch-ai/video-mme` |
| instruction | `lmarena/text-instruction-following` |

An observation qualifies when the latest snapshot of its source has a finite
value for a board row that maps to the model and that `scores.yaml` does not
ignore. Because the winning metric can differ between models, compare values
only when `provenance.source` and `provenance.metric` match.

## Result

```json
{
  "model": "anthropic/claude-opus-4-5",
  "scores": [
    {
      "axis": "coding",
      "value": 0.79,
      "state": "known",
      "provenance": {
        "source": "epoch-ai",
        "metric": "swe-bench-verified",
        "unit": "fraction",
        "as_of": "2025-12-01",
        "source_key": "claude-opus-4-5-20251101",
        "matched_by": "rule:dateless",
        "retrieved_at": 1793754000000
      },
      "reasons": []
    }
  ]
}
```

- `known`: the winning observation comes from a snapshot younger than
  `--max-age` days (default 30).
- `stale`: same, but the snapshot is older. The value is still shown. Staleness
  never demotes a higher-ranked source.
- `unknown`: no qualified observation. `value` and `provenance` are null and
  never 0. `reasons` says why for each ranked metric.

## Model mapping

Board rows keep their own names (`claude-opus-4-5-20251101_32K`,
`Claude Opus 5.5 (High)`, `GPT-6 Astra`). For each source and metric, a
canonical `<provider>/<model>` ID is mapped in this order:

1. **`scores.yaml` aliases.** When an alias exists for the model and source (and
   metric, when it is given), only the pinned keys are used.
2. **Builtin aliases** (`mapping::BUILTIN_ALIASES`), same semantics. The table
   is empty in this release. It is for names that the rules cannot reach.
3. **Deterministic rules.** Names are compared after normalization only:
   lowercase; spaces, `_` and `-` unified; `4.5` and `4-5` treated as equal.
   Trailing variant tags (`_high`, `_32K`, `(High)`, `-thinking-32k`) and
   trailing snapshot dates (`-20251101`, `-2025-11-13`) are split off.
   - `rule:exact`: the same name, with no variant tag.
   - `rule:variant`: the same name, and exactly one tagged variant on the board.
   - `rule:dateless`: the same name apart from a snapshot date. A dated ID
     never matches a *different* date.
   - When the board has an organization (or the name implies a vendor), it must
     match the ID's provider.
   - Two or more distinct board keys at the same step make the axis
     **Unknown (ambiguous)**. The axis does not fall through to a lower-ranked
     source. Add an alias to choose one.

Nothing else is tried: no prefix, substring or edit-distance matching. For
example, `openai/gpt-6` never matches `GPT-6 Astra`.

## `scores.yaml`

`<config dir>/scores.yaml` (`$ROSTR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/rostr`,
else `~/.config/rostr`). It is optional and edited by hand only. Rostr never writes it.
An invalid file stops `score query`.

```yaml
version: 1
aliases:
  # Pin the variant a source uses for a model (metric optional).
  - model: openai/gpt-5
    source: epoch-ai
    metric: swe-bench-verified
    key: gpt-5-2025-08-07_high
ignore:
  # Drop rows before matching; every given field must match.
  # model may be a wildcard; key matches a board key (case-insensitive).
  - model: anthropic/claude-opus-4-5
    source: epoch-ai
    metric: eci
  - source: lmarena
    axis: coding
values:
  # A manual value outranks every source for this model and axis.
  - model: example/in-house-model
    axis: coding
    value: 0.61
    metric: team-eval     # optional
    unit: fraction        # optional
    as_of: 2026-10-30     # optional
    note: internal suite  # optional, shown in reasons
```

Aliases and ignore rules are applied at query time, so edits take effect
without a refresh.

## Storage

Schema v3 of `rostr.db` adds `score_runs` (one row per parsed snapshot:
source, parser version, retrieval time, origin, file names with SHA-256 and
size, parser notes) and `score_rows` (board key, organization, metric, native
value, `as_of`). The latest three snapshots per source are kept. Discovery and
quota tables are unchanged. Quota accepts schema v2 or newer.

## Limitations and deferred work

- **No TUI.** Alias mapping is done in `scores.yaml`. A TUI for reviewing
  ambiguous or unmatched models is deferred to phase 2.
- **Instruction** has one source (LMArena's instruction-following category, a
  human-preference rating, not IFEval). Without an LMArena snapshot it is Unknown.
- **Multimodal**: Video-MME in Epoch's data mostly covers older open models;
  LMArena Vision is the primary source.
- **LMArena paging.** Refresh reads the precomputed `/rows` pages of the
  `latest` split, about 115 requests of 100 rows, and selects categories
  locally. The `/filter` endpoint was not used: it computes results on demand,
  took 10-60 s per page and often answered 500/504 during verification. Transient
  5xx answers are retried three times (2 s, 4 s and 8 s back-off). A source that
  still fails keeps its previous snapshot, and the command exits non-zero.
- **Aider's own leaderboard YAML** is not a separate source. Epoch's
  `aider_polyglot_external.csv` already redistributes it.
- Values are public claims from third-party boards. They do not prove what a
  given route, account or agent configuration will achieve.
