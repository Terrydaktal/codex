# Local Codex customizations

This directory makes the local Codex changes portable without relying on the
Git index, a dirty-worktree snapshot, or generated files from one release.

## Canonical checkout and archives

`/home/lewis/repos/codex` is the canonical working checkout. Keep the local
feature commits on `main`, with `origin` pointing to the fork and `upstream`
pointing to OpenAI. Preserve a known-good release with a Git tag before rebasing;
temporary upgrade checkouts should not become another permanent source copy.

Old source and Git metadata, including uncommitted and untracked files, are
preserved in `archives/*.tar.zst`. These snapshots exclude Cargo build outputs.
`/archives/` is Git-ignored, so the port audit/export and ordinary Git-aware
`rg`/`fd` searches skip it. Do not add these archives to the customization
manifest or create fresh full snapshots for every routine upgrade.

`codex-rs/target` is a symlink to the reusable build cache at
`/home/lewis/.cache/codex/cargo-target`. The installed `~/.local/bin/codex`
symlink uses the canonical checkout path. Keep `codex-code-mode-host` beside
`codex` in the release directory. Versioned compatibility symlinks may remain
while old sessions are open; they are not additional source checkouts. Prune
obsolete build artifacts separately from source/history archives.

## Upgrade workflow

From the customized checkout, port everything to a clean checkout of the next
upstream release with one command:

```sh
./scripts/port-local-customizations.py port \
  --target /home/lewis/repos/codex-vNEXT
```

The command audits the source tree, exports one patch per local feature commit to a
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

Git history on `main` is canonical. Keep each independently reviewable fix in
its own commit, with a plain-English problem, reason, implementation,
before/after behavior, and result. Do not group unrelated changes merely because
they touch the TUI. Dependent fixes stay ordered after their prerequisites.
Upstream history is not rewritten or exported.

Each export generates numbered `0001-<commit-subject>.patch` files, preserving
the commit author and complete message, plus `series` (application order) and
`bundle.json` (source commit IDs, SHA-256 checksums and exclusions). A dirty
checkout produces an explicitly named final WIP patch; commit that work into
logical fixes before making a release export. Generated schemas and lockfile
churn remain excluded. Existing schema-1 three-patch bundles can still be read.

Generate the local patch files into an empty directory with:

```sh
./scripts/port-local-customizations.py export \
  --bundle local-customizations/patches
```

That output directory is ignored: committing exports of the exporter itself
would recursively duplicate history. Keep the feature commits, tooling and
manifest in Git; regenerate the patch files after committing. The command
refuses to overwrite an existing bundle, so preserve or remove only the old
generated output before re-exporting. It never deletes source or archives.

Preflight applies patches sequentially in a disposable index, so a later fix
can edit a file introduced or changed by an earlier one. A conflict changes
neither the target files nor its real index. Successful application writes the
fully preflighted result in one operation without staging it. Review conflicts
at the feature boundary and re-create corresponding commits on the next base.

Run the offline exporter tests with:

```sh
UV_CACHE_DIR=/data/.cache/uv uv run --project scripts python -m unittest \
  discover -s scripts -p test_port_local_customizations.py -v
```

Codex v0.153 and later already persist native response-level `TokenUsageRecord`
items, including remote compactions; do not restore the superseded local
`CompactionUsage` record type when porting the runtime usage patches.

## Per-window backend by default

Ordinary local TUI launches, including resume and fork, use an embedded backend
owned by that window. They must neither discover an existing shared daemon nor
automatically start one. Closing the process releases that backend's memory;
this does not impose a memory cap while the window remains open.

Keep the default exclusion in `codex-rs/tui/src/daemon_startup.rs` and its startup
gate in `startup_orchestration.rs`. The upstream `daemon_auto_start` feature
must not override this local launch policy, even when enabled in configuration.
Explicit `codex agents` and `--remote` launches still select a shared server;
`--no-daemon` retains its existing explicit opt-out and conflict checks.
The policy does not stop existing daemons or change their active sessions.

Preserve the new/resume/fork policy tests in `daemon_startup_tests.rs`, the real
PTY/socket checks in `tests/suite/daemon_compatibility.rs`, and the explicit
remote reconnect test in `tests/suite/reconnect.rs` when porting this patch.

## Outdated running-binary titles (Linux)

The TUI reads the running native executable's GNU ELF build ID through
`/proc/self/exe` once at startup. It compares that ID with the launch target's
ID, not its version, Git revision, inode or timestamps. File metadata is only
a cache key. Bounded reads of ELF program headers avoid reading the entire
binary or loading/executing a candidate.

The default launcher is the original invocation path (`argv[0]`, with PATH
lookup for a bare command). Keep that path unresolved: resolving it once at
startup would miss later symlink retargeting. Directory-link indirections and
static shell `exec TARGET "$@"` wrappers are followed on each background check.
The captured working directory anchors relative paths even after thread switches.

Wrappers that obscure the native invocation path can export
`CODEX_LAUNCHER_PATH=/unresolved/path/to/launcher`. Arbitrary shell, Python and
JavaScript programs cannot be inferred safely. For those, the launcher can also
export `CODEX_LAUNCHER_EXECUTABLE=/unresolved/path/to/native/codex` as its explicit
native-target contract. No wrapper or candidate is executed by the monitor.
Without a readable, identifiable native target the result is unknown; it never
clears an established stale marker.

Filesystem events are filtered to launcher/target paths and their symlink
components, debounced for one second, with a 30-second fallback. Identity reads
run on blocking workers, never on redraws, keystrokes or output callbacks.
Only changed known identities notify the title renderer. Restoring the running
build clears the marker. The monitor is window-owned and stops on TUI exit.

The renderer strikes only the semantic `app-name` segment, using U+0336 (plain
Unicode), and appends that label while stale if it was omitted. It respects
`tui.terminal_title = []`. Other project/thread/status text and XFCE's separate
Terminal label are untouched. tmux can forward this OSC title with `set-titles`
enabled; neither terminal replacement nor a changed symlink updates the running
Codex binary inside tmux.

Keep these reviewable layers together when porting: `running_build/elf.rs` and
`launcher.rs` (identity/resolution), `monitor.rs` and `watch.rs` (background
lifecycle), and the small window/status-surface hooks (title rendering).
Regression coverage lives in `running_build_tests.rs`,
`chatwidget/tests/terminal_title.rs`, `app/tests/active_reconnect_tests.rs`, and
the private tmux test in `tests/suite/running_build.rs`. No SQLite, history, clipboard, or model behavior
is changed by this feature.

## GNU/Linux allocator retention

The CLI configures glibc's mmap threshold at 128 KiB at the beginning of main,
before build-info initialization or any threads: [glibc marks `mallopt`
MT-Unsafe](https://sourceware.org/glibc/manual/latest/html_node/Malloc-Tunable-Parameters.html).
Large, short-lived history/image buffers then
release their mappings on free instead of accumulating in arenas pinned by
small live allocations. A window-owned worker calls `malloc_trim(0)` every
60 seconds to release other free arena pages. Start only that worker after arg0
helper dispatch, and stop it promptly on CLI exit.
Neither action runs in redraw/input callbacks or changes child-tool environment
variables. Sandbox/exec helpers do not start this worker. Musl, macOS and Windows
keep their existing allocator policies.

This addresses measured freed-memory retention, not every possible live-memory
growth or an unlimited history size. It cannot shrink live objects or change
already-running old binaries; restart those windows to use the new policy.
The isolated subprocess regression in `cli/src/memory_allocator_tests.rs`
compares default glibc retention against the configured threshold, exercises
background reclamation below that threshold, and checks worker shutdown.
The real CLI/preload regression in `cli/tests/memory_allocator.rs` checks that
configuration runs while the process still has exactly one thread.

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

Capture clipboard images once as inline PNG data URLs (including the WSL
PowerShell fallback), not temporary PNG files or borrowed clipboard file paths.
Keep those URLs in the existing attachment state through queued messages,
draft handoff, restoration, and submission. New clipboard snapshots insert an
atomic `[Image #N]` at the cursor, matching drag-and-drop, rather than a separate
row above the draft. Delete the matching bytes when its placeholder is removed;
preserve numbering through mixed local/clipboard images, external editing, and
merging queued drafts. Existing remote URLs without inline elements retain their
row presentation. Ordinary local file attachments remain path-based. Preserve
coverage for both paste routes and image-only input.

Selection must not write to the clipboard. Only explicit copy shortcuts
(Ctrl+C, Ctrl+Shift+C, and Cmd+C) copy selected transcript or composer text;
mouse release, right-click, and Enter do not. Enter is reserved while selecting
so it cannot accidentally submit a draft or rewind a turn. Preserve the shared
`transcript_view/input.rs` policy in fullscreen,
Ctrl+T, and session previews. The upstream `tui.copy_on_select` config remains
loadable for compatibility but is deliberately not used by this build.
Regressions live in `transcript_view/manual_copy_tests.rs`,
`transcript_view/copy_delivery_tests.rs`, and the composer/owned-transcript tests.

The user's main view uses terminal-owned history and selection through
`tui.fullscreen_transcript = false` in `$CODEX_HOME/config.toml`, alongside
`tui.copy_on_select = "never"` and `tui.right_click_paste = "off"`.
Preserve these preferences during upgrades;
do not force fullscreen ownership when starting or resuming a chat. Ctrl+T
remains a keyboard-operated viewer and keeps the explicit-copy-only policy.

Pointer input belongs to the terminal in this custom build, including composer,
transcript, usage, onboarding, and session-preview screens. Production terminal
setup must disable mouse reporting, and screen/overlay/editor/suspend transitions
must not re-enable it. `tui/event_stream.rs` drops residual/native mouse records
before any UI receives them, so clicking cannot reposition the typing cursor or
trigger Codex selection, links, or right-click paste. Preserve bracketed paste,
keyboard enhancement, and Ctrl+V image handling. Regression coverage includes
the rendered `terminal_owned_mouse_keyboard_cursor` snapshot and the isolated
owned-screen lifecycle test. Mouse support inside upstream widgets is retained
but intentionally unreachable from the production event stream.

The shared tmux input probe in `tui/tmux.rs` must prefer an executable
`~/.local/bin/tmux`, including a symlink to a user build, before its trusted
system fallback. Losing this probe hides `extended-keys-format = csi-u` and
prevents Codex from requesting the mode that preserves Shift+Enter with
`extended-keys = on`. Keep automatic helpers' global system-only lookup and the
probe's sanitized child PATH; do not replace it with ambient PATH discovery.
The containing-pane and user-installation regressions live in `tui/tmux_tests.rs`.

## Textarea wrapping

Codex v0.153's upstream textarea wrapper supersedes the local v0.147 line-wrap
workaround. Do not export the old manual grapheme splitter or its snapshots;
rely on upstream `wrapping::wrapped_lines` so future wrapping fixes continue to
arrive with normal upgrades.

Activity preview rows must not expand on mouse clicks. Preserve ordinary text
terminal-owned selection and the advertised Ctrl+T full-transcript shortcut; its input and
rendering regression lives in `app/owned_transcript_input_tests.rs`.

## Terminal title separators

Terminal-title metadata uses ` - ` separators, including action-required titles
and `/title` previews. Keep plain spaces around the activity spinner. Preserve
the idle/working/missing-thread regression in `chatwidget/tests/terminal_title.rs`
and the title-preview snapshots when porting this preference to another release.

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
