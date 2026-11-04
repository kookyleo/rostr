# Rostr

**AI Model Resource Registry**

Rostr is a lightweight CLI that gives AI agents a local, deterministic view of
the models they can use: which models are configured, what quota observations
exist for them, and what public benchmarks say about them.

Queries read local files and `rostr.db` only and use no model tokens. Rostr
does not perform inference, proxy model requests, route traffic, or pick a
model for you; callers apply their own policy to the data.

## Status

| Command | State |
| --- | --- |
| `rostr discover` | Implemented: finds local agents and models, saves your model scope |
| `rostr quota refresh --local` / `import` / `query` | Implemented: local quota observations with explicit Unknown states |
| `rostr quota init --claude` | Implemented: wraps the Claude Code `statusLine` to capture subscription rate limits |
| `rostr score refresh` / `import` / `query` / `sources` | Implemented: public benchmark scores on five axes |

### Roadmap

- Score phase 2: a TUI to review unmatched or ambiguous model mappings
  (today: `scores.yaml` aliases), and more verified sources for the
  `instruction` and `multimodal` axes.
- More native local quota observers (Codex, Grok Build and the Claude Code
  status line today).

## Install

```sh
cargo install rostr
```

## Usage

```sh
rostr discover            # scan agents, choose models in a TUI, review, save
rostr discover --dry-run  # print the merged result and warnings; save nothing
rostr discover --yes      # save the default selection without the TUI
```

### Local quota observations

```sh
rostr quota init --claude   # once: wrap Claude Code's statusLine (undo: --uninstall)
rostr quota refresh --local
# Optional export import (for example, an existing Claude statusline snapshot):
rostr quota import /path/to/quota-snapshot.yaml
rostr quota query --agent claude-code --metric subscription_usage --unit percent \
  --min-remaining 20 --max-age 120 --json
```

Claude Code keeps no quota on disk; its official channel is the status line
command. `rostr quota init --claude` wraps your existing `statusLine.command` in
`~/.claude/settings.json` (or installs a minimal one), keeping your status bar
working. Each update writes the `rate_limits` windows, and nothing else, to a fixed
snapshot that `refresh --local` reads. Rostr is not a proxy. Re-running is safe;
managed policies that disable or override status lines stop it without writing.
The account label is yours to assign (`--account`).

Quota import supports JSON/YAML snapshots and existing Claude statusline exports.
It never starts agents, reads credentials or connects to a network. Query is
read-only; unknown or expired balances never become zero or unlimited. See the
[native file sources, export format, fixtures and limitations](docs/quota.md).
Codex rollout windows and Grok Build billing log periods are read directly from
disk. Missing data and unresolved account identity remain Unknown; refresh never
starts agents, reads auth values, or connects to a network.

### Public benchmark scores

```sh
rostr score refresh        # the only networked command: fixed HTTPS URLs, TLS verified
rostr score sources        # sources, licenses, rank table, last snapshots
rostr score query          # selected models; or --model <provider>/<model>, --axis, --json
```

Axes: `general`, `coding`, `reasoning`, `multimodal`, `instruction`. For each
model and axis, the first qualified observation from a fixed rank table of
[Epoch AI](https://epoch.ai/benchmarks) and
[LMArena](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset)
metrics wins (both CC BY 4.0). Sources are never averaged. Each result is the
board's native value with `known`, `stale` or `unknown` state and provenance:
source, metric, `as_of`, board key and how the model was matched. Missing or
ambiguous models are `unknown` with a null value, never 0. Model IDs are mapped
through `scores.yaml` aliases, then builtin aliases, then deterministic name
rules. There is no fuzzy matching. See [scores, sources, mapping and
`scores.yaml`](docs/score.md).

### Model discovery

`rostr discover` reads local configuration only. It runs no agent, sends no
network request and uses no model tokens. Supported agents:

| Agent       | Sources |
| ----------- | ------- |
| Claude Code | `~/.claude/settings.json`, `~/.claude.json`, `ANTHROPIC_*` variables, built-in aliases |
| Codex       | `$CODEX_HOME/config.toml` (including profiles), `models_cache.json` |
| OpenCode    | `$XDG_CONFIG_HOME/opencode/opencode.json[c]` |
| Grok CLI    | `~/.grok/config.toml`, `models_cache.json` |
| Gemini CLI  | `~/.gemini/settings.json`, `GEMINI_MODEL` |
| Pi          | `$PI_CODING_AGENT_DIR` or `~/.pi/agent`: `settings.json` (default and enabled models), `models.json` |

TUI keys: `↑↓` (or `j`/`k`) navigate, `Space` toggle a model or a whole
provider, `A` toggle all, `←→` (or `h`/`l`) fold, `+` add a model ID or a
wildcard pattern, `D` remove a model that the scan did not find (or a pattern).
`?` shows every key.

`Enter` opens a summary of what the save changes: patterns, models that become
selected or unselected, added and removed models, and what goes into `rostr.db`.
A second `Enter` (or `y`) writes the files; `Esc` returns to the list. With no
edits and nothing new, `Enter` exits without writing. `Esc` exits at once when
there are no edits; otherwise it asks first, and a second `Esc` discards them.

### Model scope

The choice is saved to `~/.config/rostr/scope.yaml` (or `$XDG_CONFIG_HOME/rostr`,
or `$ROSTR_CONFIG_DIR`). The file is the source of truth for user intent and is
safe to edit by hand:

```yaml
version: 1
patterns:
  - "anthropic/*"
  - "openai/*codex*"
exceptions:
  - id: anthropic/claude-haiku-4-5
    selected: false
  - id: xai/grok-4.7
    selected: true
```

- `patterns`: a model is selected when a pattern matches its canonical
  `<provider>/<model>` ID. The first run creates one `<provider>/*` pattern for
  each discovered provider.
- `exceptions`: explicit choices that win over patterns. `selected: false`
  keeps a model excluded even when a pattern matches it; `selected: true` pins a
  model that no pattern matches. A toggle in the TUI writes an exception only
  when the choice differs from the patterns.
- `Space` on a provider sets `<provider>/*`, or removes every pattern of that
  provider, and drops the provider's exceptions.
- Rostr validates, deduplicates and atomically replaces the file on save. An
  invalid file stops `rostr discover`; Rostr does not overwrite it.
- `overrides.yaml` is never written by `rostr discover`.

### Model database

`~/.local/share/rostr/rostr.db` (or `$XDG_DATA_HOME/rostr`, or
`$ROSTR_DATA_DIR`) is a SQLite database of collected data:

| Table           | Content |
| --------------- | ------- |
| `models`        | Every known model, first and last seen time, and its resolved state (`selected`, `reason`) |
| `model_sources` | Which agent reported each model, and why (default model, model list, ...) |
| `scans`         | Discovery history |
| quota tables    | Local quota observations (schema v2, see [docs/quota.md](docs/quota.md)) |
| `score_runs`, `score_rows` | Benchmark board snapshots (schema v3, see [docs/score.md](docs/score.md)) |

A model that a later scan does not find stays in the database. The TUI marks
it `missing` (dim row) with its last known agents, and `D` removes it. Models
that the database does not know yet are marked `new` (green row).
The resolved state is derived from `scope.yaml` on every save; it is never a
second copy of user intent. `rostr discover --dry-run` lists every model with
its agents, in parentheses.

## Development

```sh
cargo check
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Source: [github.com/kookyleo/rostr](https://github.com/kookyleo/rostr)

## License

Licensed under [Apache-2.0](LICENSE).
