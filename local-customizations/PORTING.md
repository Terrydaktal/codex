# Local Codex customizations

This directory makes the local Codex changes portable without relying on the
Git index, a dirty-worktree snapshot, or generated files from one release.

## Upgrade workflow

From the customized checkout, port everything to a clean checkout of the next
upstream release with one command:

```sh
./scripts/port-local-customizations.py port \
  --target /home/lewis/repos/codex-vNEXT
```

The command audits the source tree, exports three ordered patches to a
temporary bundle, preflights all of them against the target, applies them
without staging or committing, and records the target's original `HEAD` as the
base for the following upgrade. It refuses to modify a dirty target. If an
exact patch no longer applies, it automatically attempts a three-way merge
through a temporary index, preserving the target's real index.

Regenerate version-specific files and format the result from either checkout:

```sh
./scripts/port-local-customizations.py finalize \
  --target /home/lewis/repos/codex-vNEXT
```

Then run the focused tests relevant to any conflicts resolved during the port.
Because the customization changes `core`, `protocol`, and app-server behavior,
a complete `just test` is the final validation when practical.

## Patch layout

The generated bundle contains:

1. `01-runtime-and-protocol.patch` for task-summary usage events, replay,
   analytics, account rate limits, runtime metrics, and cross-process rollout
   append serialization. Codex v0.153 and later already persist native
   response-level `TokenUsageRecord` items, including remote compactions; do
   not restore the superseded local `CompactionUsage` record type.
2. `02-tui.patch` for task summaries, the shared weekly ledger, workspace
   statistics, multi-agent response aggregation, historical replay, Ctrl+V
   image paste, direct Max/Ultra reasoning choices, and legacy history mode for
   embedded TUI sessions.
3. `03-tools-and-porting.patch` for the bootstrap and porting tools.

The grouping limits a TUI conflict from obscuring the lower-level runtime
changes and gives `git apply` smaller, identifiable failure boundaries.

## Session-history safety

Embedded TUI sessions explicitly request `ThreadHistoryMode::Legacy` when
starting a thread, which keeps their canonical history in append-only JSONL
instead of SQLite-backed paginated storage. Remote app-server sessions continue
to request paginated history because the remote server owns the canonical
writer. The relevant TUI policy is isolated in
`thread_start_params_from_config` in `codex-rs/tui/src/app_server_session.rs`.

Local rollout writer locks are shared after creation and resume, so multiple
Codex windows can keep the same chat open and append safely. Each append still
takes the per-rollout exclusive `.append.lock`, which serializes JSONL records
without restoring the upstream one-live-writer restriction. Preserve both the
shared lock downgrade and recorder append lock when resolving future rollout or
thread-store conflicts.

Legacy replay also recognizes rollouts that contain canonical paginated
`ItemCompleted(UserMessage)` records but no legacy `UserMessage` events. This
compatibility path expands the materialized items through `HasLegacyEvent`
before feeding `ThreadHistoryBuilder`; it preserves transcripts converted from
paginated to legacy storage without duplicating normal legacy or live mixed
event streams. Keep the focused regression in
`codex-rs/app-server/src/request_processors_tests.rs` when porting.

The rollout recorder also serializes paginated ordinal assignment and append
across processes with a per-rollout `.append.lock`. Its real subprocess
regression test lives in `codex-rs/rollout/src/recorder_tests.rs`. When porting,
run the `codex-rollout` tests and the focused embedded/remote history-mode TUI
tests before rebuilding Codex.

## Multi-agent task usage

The task summary belongs to the primary turn, but app-server token-usage
notifications remain thread-scoped. `app/task_usage_aggregation.rs` bridges
that boundary: every response is journaled to the shared weekly ledger as soon
as its notification reaches the app, including responses from inactive agent
threads. Responses from threads discovered through sub-agent activity or
collaboration receiver lists are additionally deduplicated by their stable
response snapshot id and attached to the currently active primary turn.

On primary-turn completion, `chatwidget/task_usage.rs` adds those descendant
input, cached-input, output, and reasoning totals to the primary thread's own
cumulative delta. It also sums each response's model-specific credit estimate,
so child usage is not silently priced as the root model when agents differ.
Keep both focused regressions in `app/task_usage_aggregation_tests.rs` and
`chatwidget/tests/usage.rs` when porting this behavior.

## Model credit rates and reasoning picker

The runtime and bootstrap script use the published GPT-6 credit rates per
million tokens: Astra `250/25/1250`, Sol `50/5/250`, and Luna
`2.5/0.25/12.5` for uncached input, cached input, and output respectively.
Keep GPT-5.6 mappings separate so historical rollouts retain the rates that
applied to their recorded model IDs. The exact-rate regressions live in
`chatwidget/task_usage_tests.rs`.

The normal reasoning picker contains Max and Ultra directly; do not restore a
separate “More reasoning” submenu. Advanced choices still route through the
advanced-reasoning action so warnings, plan-mode scope, and persistence remain
unchanged.

## Clipboard image paste

Ctrl+V, Ctrl+Shift+V, and Ctrl+Alt+V first probe the clipboard for an image and
attach it when present. Terminal bracketed-paste events also perform that probe
before accepting the clipboard's text payload, because some terminals never
deliver Ctrl+V as a key event. Preserve both paths and the injectable image
paster used by the regression tests; Alt+V alone is not an image-paste shortcut.

Selection must not write to the clipboard. Only explicit copy shortcuts
(Ctrl+C, Ctrl+Shift+C, and Cmd+C) copy selected transcript or composer text;
mouse release, right-click, and Enter do not. Enter is reserved while selecting
so it cannot accidentally submit a draft or rewind a turn. Preserve the shared
`transcript_view/input.rs` policy in fullscreen,
Ctrl+T, and session previews. The upstream `tui.copy_on_select` config remains
loadable for compatibility but is deliberately not used by this build.
Regressions live in `transcript_view/manual_copy_tests.rs`,
`transcript_view/copy_delivery_tests.rs`, and the composer/owned-transcript tests.

## Textarea wrapping

Codex v0.153's upstream textarea wrapper supersedes the local v0.147 line-wrap
workaround. Do not export the old manual grapheme splitter or its snapshots;
rely on upstream `wrapping::wrapped_lines` so future wrapping fixes continue to
arrive with normal upgrades.

Activity preview rows must not expand on mouse clicks. Preserve ordinary text
selection and the advertised Ctrl+T full-transcript shortcut; its input and
rendering regression lives in `app/owned_transcript_input_tests.rs`.

## Files deliberately not ported

`codex-rs/Cargo.lock` is regenerated for the target release. The source tree's
lockfile also contains release-build version churn that is unrelated to the
customization.

Everything below `codex-rs/app-server-protocol/schema/` is generated from the
protocol source. Stable and experimental artifacts are regenerated by
`finalize` instead of being merged across releases. The generated Python v2
bindings in `sdk/python/src/openai_codex/generated/v2_all.py` follow the same
rule.

Pending `*.snap.new` files are test residue. Accepted snapshot fixtures are
listed explicitly in `manifest.json` and are portable.

Every other untracked file must be listed in `portable_untracked_paths`.
`audit`, `export`, and `port` fail if they find an unclassified file, preventing
a new source module from being silently omitted.

## Useful commands

Inspect what would be carried without writing anything:

```sh
./scripts/port-local-customizations.py audit
```

Keep a reviewable bundle instead of applying immediately:

```sh
./scripts/port-local-customizations.py export --bundle /tmp/codex-local-port
./scripts/port-local-customizations.py check \
  --bundle /tmp/codex-local-port \
  --target /home/lewis/repos/codex-vNEXT
./scripts/port-local-customizations.py apply \
  --bundle /tmp/codex-local-port \
  --target /home/lewis/repos/codex-vNEXT
```

If preflight reports a conflict, no target file has been changed. Resolve the
reported upstream touchpoint in the new checkout, then rerun the audit and
tests there. Do not add generated schema or lockfile conflicts to the portable
patch.
