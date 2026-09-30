#!/usr/bin/env python3

import argparse
import fnmatch
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime
from datetime import timezone
from pathlib import Path


MANIFEST_PATH = Path("local-customizations/manifest.json")
BUNDLE_METADATA = "bundle.json"
PATCH_GROUPS = (
    "01-runtime-and-protocol",
    "02-tui",
    "03-tools-and-porting",
)

HELP_TEXT = """\
NAME
    port-local-customizations.py - carry the local Codex patch to a new release

SYNOPSIS
    port-local-customizations.py audit [--source PATH]
    port-local-customizations.py export --bundle PATH [--source PATH]
    port-local-customizations.py check --bundle PATH --target PATH
    port-local-customizations.py apply --bundle PATH --target PATH
    port-local-customizations.py port --target PATH [--source PATH]
    port-local-customizations.py finalize --target PATH

DESCRIPTION
    Exports source-only local changes relative to the upstream base recorded in
    local-customizations/manifest.json. Generated schemas, Cargo.lock churn,
    and pending snapshot residue are excluded. The port command combines audit,
    export, preflight, and apply without changing either repository's real Git
    index. When an exact apply fails, it safely performs a three-way merge into
    a temporary index so conflicts fail preflight without touching the target.

OPTIONS
    -h, --help
        Show this help text.
    --source PATH
        Customized Codex checkout. Defaults to the checkout containing this
        script.
    --target PATH
        Clean checkout of the upstream release receiving the customization.
    --bundle PATH
        Empty or nonexistent directory used for the portable patch bundle.

OPERATION
    audit       Classify tracked and untracked changes and fail on omissions.
    export      Write three ordered patches plus checksummed metadata.
    check       Verify a bundle and preflight every patch against a clean target.
    apply       Preflight and atomically apply all patches to a clean target.
    port        Export to a temporary bundle and apply it to a clean target.
    finalize    Regenerate lock/schema artifacts and run repository formatting.

EXAMPLES
    ./scripts/port-local-customizations.py audit
    ./scripts/port-local-customizations.py port --target ../codex-vNEXT
    ./scripts/port-local-customizations.py finalize --target ../codex-vNEXT
    ./scripts/port-local-customizations.py export --bundle /tmp/codex-port

FILES
    local-customizations/manifest.json
        Upstream base, generated exclusions, and explicit untracked sources.
    BUNDLE/bundle.json
        Source metadata, file inventory, patch order, and SHA-256 hashes.
    BUNDLE/*.patch
        Ordered source patches consumed by check and apply.

PATHS
    All paths stored in the manifest and bundle are repository-relative POSIX
    paths. Bundle output should normally be outside the source checkout.

SECURITY NOTES
    apply and port require a clean target and run git apply without hooks,
    commits, or commands from the patch. Three-way fallback uses a disposable
    index so the real index remains unchanged. finalize runs trusted project
    commands from the target checkout and should only be used on a reviewed
    Codex source tree.

EXIT STATUS
    0   The requested operation completed successfully.
    1   Audit, bundle validation, Git preflight, apply, or regeneration failed.
    2   Command-line usage was invalid.

AUTHORS
    Local Codex customization maintained by Terrydaktal.
"""


class PortError(RuntimeError):
    pass


@dataclass(frozen=True)
class Audit:
    base_commit: str
    version: str
    portable_tracked: tuple[str, ...]
    portable_untracked: tuple[str, ...]
    generated_changes: tuple[str, ...]
    ignored_untracked: tuple[str, ...]
    unknown_untracked: tuple[str, ...]
    missing_portable_paths: tuple[str, ...]

    def ensure_portable(self) -> None:
        errors = []
        if self.unknown_untracked:
            errors.append(
                "unclassified untracked files:\n  "
                + "\n  ".join(self.unknown_untracked)
            )
        if self.missing_portable_paths:
            errors.append(
                "manifest paths that do not exist:\n  "
                + "\n  ".join(self.missing_portable_paths)
            )
        if errors:
            raise PortError("\n".join(errors))


@dataclass(frozen=True)
class Preflight:
    target: Path
    patch_paths: tuple[Path, ...]
    use_three_way: bool


def run(
    args: list[str],
    *,
    cwd: Path,
    check: bool = True,
    capture_output: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(
        args,
        cwd=cwd,
        check=False,
        stdout=subprocess.PIPE if capture_output else None,
        stderr=subprocess.PIPE if capture_output else None,
        env=env,
    )
    if check and completed.returncode != 0:
        stderr = (completed.stderr or b"").decode(errors="replace").strip()
        command = " ".join(args)
        detail = f"\n{stderr}" if stderr else ""
        raise PortError(f"command failed ({completed.returncode}): {command}{detail}")
    return completed


def git(repo: Path, *args: str, check: bool = True) -> bytes:
    return run(["git", *args], cwd=repo, check=check).stdout


def repository_root(path: Path) -> Path:
    candidate = path.expanduser().resolve()
    if not candidate.is_dir():
        raise PortError(f"repository path does not exist: {candidate}")
    root = git(candidate, "rev-parse", "--show-toplevel").decode().strip()
    return Path(root).resolve()


def default_source() -> Path:
    return Path(__file__).resolve().parents[1]


def load_manifest(repo: Path) -> dict:
    path = repo / MANIFEST_PATH
    try:
        manifest = json.loads(path.read_text())
    except FileNotFoundError as error:
        raise PortError(f"missing customization manifest: {path}") from error
    except json.JSONDecodeError as error:
        raise PortError(f"invalid customization manifest: {error}") from error
    if manifest.get("schema_version") != 1:
        raise PortError(f"unsupported manifest schema in {path}")
    return manifest


def nul_paths(raw: bytes) -> tuple[str, ...]:
    return tuple(
        sorted(
            item.decode(errors="surrogateescape") for item in raw.split(b"\0") if item
        )
    )


def is_at_or_below(path: str, root: str) -> bool:
    root = root.rstrip("/")
    return path == root or path.startswith(f"{root}/")


def is_generated(path: str, manifest: dict) -> bool:
    return any(
        is_at_or_below(path, generated) for generated in manifest["generated_paths"]
    )


def is_ignored_untracked(path: str, manifest: dict) -> bool:
    return any(
        fnmatch.fnmatchcase(path, pattern)
        for pattern in manifest["ignored_untracked_globs"]
    )


def tracked_by_git(repo: Path, path: str) -> bool:
    result = git(repo, "ls-files", "--error-unmatch", "--", path, check=False)
    return bool(result)


def audit_repository(repo: Path) -> tuple[dict, Audit]:
    manifest = load_manifest(repo)
    base = manifest["upstream"]["base_commit"]
    git(repo, "cat-file", "-e", f"{base}^{{commit}}")

    changed = nul_paths(
        git(
            repo,
            "diff",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-renames",
            base,
            "--",
        )
    )
    portable_tracked = tuple(
        path for path in changed if not is_generated(path, manifest)
    )
    generated_changes = tuple(path for path in changed if is_generated(path, manifest))

    untracked = nul_paths(git(repo, "ls-files", "--others", "--exclude-standard", "-z"))
    expected = set(manifest["portable_untracked_paths"])
    portable_untracked = tuple(path for path in untracked if path in expected)
    ignored_untracked = tuple(
        path
        for path in untracked
        if path not in expected
        and (is_generated(path, manifest) or is_ignored_untracked(path, manifest))
    )
    unknown_untracked = tuple(
        path
        for path in untracked
        if path not in expected
        and not is_generated(path, manifest)
        and not is_ignored_untracked(path, manifest)
    )
    missing_portable_paths = tuple(
        sorted(
            path
            for path in expected
            if not (repo / path).exists() and not tracked_by_git(repo, path)
        )
    )

    audit = Audit(
        base_commit=base,
        version=manifest["upstream"]["version"],
        portable_tracked=portable_tracked,
        portable_untracked=portable_untracked,
        generated_changes=generated_changes,
        ignored_untracked=ignored_untracked,
        unknown_untracked=unknown_untracked,
        missing_portable_paths=missing_portable_paths,
    )
    return manifest, audit


def patch_group(path: str) -> str:
    if path.startswith("codex-rs/tui/"):
        return "02-tui"
    if path.startswith("scripts/") or path.startswith("local-customizations/"):
        return "03-tools-and-porting"
    return "01-runtime-and-protocol"


def new_file_patch(repo: Path, path: str) -> bytes:
    result = run(
        ["git", "diff", "--binary", "--no-index", "--", "/dev/null", path],
        cwd=repo,
        check=False,
    )
    if result.returncode not in (0, 1):
        stderr = result.stderr.decode(errors="replace").strip()
        raise PortError(f"could not export untracked file {path}: {stderr}")
    if not result.stdout:
        raise PortError(f"untracked path produced an empty patch: {path}")
    return result.stdout


def tracked_patch(repo: Path, base: str, paths: list[str]) -> bytes:
    if not paths:
        return b""
    return git(
        repo,
        "diff",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-renames",
        base,
        "--",
        *paths,
    )


def ensure_empty_bundle(path: Path) -> Path:
    bundle = path.expanduser().resolve()
    if bundle.exists():
        if not bundle.is_dir():
            raise PortError(f"bundle path is not a directory: {bundle}")
        if any(bundle.iterdir()):
            raise PortError(f"bundle directory is not empty: {bundle}")
    bundle.mkdir(parents=True, exist_ok=True)
    return bundle


def export_bundle(source: Path, bundle_path: Path) -> Path:
    source = repository_root(source)
    _, audit = audit_repository(source)
    audit.ensure_portable()
    bundle = ensure_empty_bundle(bundle_path)

    patches = []
    for group in PATCH_GROUPS:
        tracked_paths = [
            path for path in audit.portable_tracked if patch_group(path) == group
        ]
        untracked_paths = [
            path for path in audit.portable_untracked if patch_group(path) == group
        ]
        content = bytearray(tracked_patch(source, audit.base_commit, tracked_paths))
        for path in untracked_paths:
            content.extend(new_file_patch(source, path))
        if not content:
            continue

        filename = f"{group}.patch"
        patch_path = bundle / filename
        patch_path.write_bytes(content)
        patches.append(
            {
                "file": filename,
                "sha256": hashlib.sha256(content).hexdigest(),
                "bytes": len(content),
                "tracked_paths": tracked_paths,
                "untracked_paths": untracked_paths,
            }
        )

    if not patches:
        raise PortError("no portable customization changes were found")

    metadata = {
        "schema_version": 1,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "source": {
            "upstream_base": audit.base_commit,
            "upstream_version": audit.version,
            "head": git(source, "rev-parse", "HEAD").decode().strip(),
        },
        "patches": patches,
        "excluded": {
            "generated_changes": list(audit.generated_changes),
            "ignored_untracked": list(audit.ignored_untracked),
        },
    }
    (bundle / BUNDLE_METADATA).write_text(json.dumps(metadata, indent=2) + "\n")
    return bundle


def load_bundle(bundle_path: Path) -> tuple[Path, dict, list[Path]]:
    bundle = bundle_path.expanduser().resolve()
    metadata_path = bundle / BUNDLE_METADATA
    try:
        metadata = json.loads(metadata_path.read_text())
    except FileNotFoundError as error:
        raise PortError(f"missing bundle metadata: {metadata_path}") from error
    except json.JSONDecodeError as error:
        raise PortError(f"invalid bundle metadata: {error}") from error
    if metadata.get("schema_version") != 1:
        raise PortError(f"unsupported bundle schema in {metadata_path}")

    patch_paths = []
    for entry in metadata.get("patches", []):
        relative_path = Path(entry["file"])
        if relative_path.is_absolute() or ".." in relative_path.parts:
            raise PortError(f"unsafe patch path in bundle: {relative_path}")
        patch_path = bundle / relative_path
        try:
            content = patch_path.read_bytes()
        except FileNotFoundError as error:
            raise PortError(f"missing bundle patch: {patch_path}") from error
        actual_hash = hashlib.sha256(content).hexdigest()
        if actual_hash != entry["sha256"]:
            raise PortError(f"bundle patch checksum mismatch: {patch_path}")
        patch_paths.append(patch_path)
    if not patch_paths:
        raise PortError(f"bundle has no patches: {bundle}")
    return bundle, metadata, patch_paths


def ensure_clean_target(target: Path) -> Path:
    target = repository_root(target)
    status = git(target, "status", "--porcelain=v1", "--untracked-files=all")
    if status:
        rendered = status.decode(errors="replace").rstrip()
        raise PortError(f"target worktree is not clean: {target}\n{rendered}")
    return target


@contextmanager
def temporary_git_index(repo: Path):
    index_path = Path(git(repo, "rev-parse", "--git-path", "index").decode().strip())
    if not index_path.is_absolute():
        index_path = repo / index_path
    if not index_path.is_file():
        raise PortError(f"target Git index does not exist: {index_path}")

    with tempfile.TemporaryDirectory(prefix="codex-port-index-") as temporary:
        temporary_index = Path(temporary) / "index"
        shutil.copy2(index_path, temporary_index)
        env = os.environ.copy()
        env["GIT_INDEX_FILE"] = str(temporary_index)
        yield env


def preflight_bundle(bundle_path: Path, target_path: Path) -> Preflight:
    target = ensure_clean_target(target_path)
    _, _, patch_paths = load_bundle(bundle_path)
    direct_error = None
    for patch_path in patch_paths:
        try:
            run(
                [
                    "git",
                    "apply",
                    "--check",
                    "--whitespace=nowarn",
                    str(patch_path),
                ],
                cwd=target,
            )
        except PortError as error:
            direct_error = PortError(
                f"patch does not apply: {patch_path.name}\n{error}"
            )
            break
    if direct_error is None:
        return Preflight(
            target=target,
            patch_paths=tuple(patch_paths),
            use_three_way=False,
        )

    with temporary_git_index(target) as env:
        three_way = run(
            [
                "git",
                "apply",
                "--3way",
                "--cached",
                "--whitespace=nowarn",
                *(str(path) for path in patch_paths),
            ],
            cwd=target,
            check=False,
            env=env,
        )
    if three_way.returncode == 0:
        return Preflight(
            target=target,
            patch_paths=tuple(patch_paths),
            use_three_way=True,
        )

    three_way_error = three_way.stderr.decode(errors="replace").strip()
    raise PortError(
        f"direct preflight failed:\n{direct_error}\n"
        f"three-way preflight also failed:\n{three_way_error}"
    ) from direct_error


def workspace_version(repo: Path) -> str:
    cargo_toml = (repo / "codex-rs/Cargo.toml").read_text()
    match = re.search(
        r"(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*\"([^\"]+)\"",
        cargo_toml,
    )
    if not match:
        raise PortError(
            "could not determine workspace version from codex-rs/Cargo.toml"
        )
    return match.group(1)


def update_target_base(target: Path, base_commit: str) -> None:
    path = target / MANIFEST_PATH
    manifest = load_manifest(target)
    manifest["upstream"] = {
        "base_commit": base_commit,
        "version": workspace_version(target),
    }
    path.write_text(json.dumps(manifest, indent=2) + "\n")


def apply_bundle(bundle_path: Path, target_path: Path) -> Path:
    preflight = preflight_bundle(bundle_path, target_path)
    target = preflight.target
    target_base = git(target, "rev-parse", "HEAD").decode().strip()
    command = ["git", "apply"]
    if preflight.use_three_way:
        command.append("--3way")
    command.extend(
        [
            "--whitespace=nowarn",
            *(str(path) for path in preflight.patch_paths),
        ]
    )
    if preflight.use_three_way:
        with temporary_git_index(target) as env:
            run(command, cwd=target, env=env)
    else:
        run(command, cwd=target)
    cached_diff = run(
        ["git", "diff", "--cached", "--quiet"],
        cwd=target,
        check=False,
    )
    if cached_diff.returncode != 0:
        raise PortError("port unexpectedly changed the target Git index")
    update_target_base(target, target_base)
    _, audit = audit_repository(target)
    audit.ensure_portable()
    return target


def print_audit(repo: Path, audit: Audit) -> None:
    print(f"repository: {repo}")
    print(f"upstream base: {audit.base_commit}")
    print(f"upstream version: {audit.version}")
    print(f"portable tracked files: {len(audit.portable_tracked)}")
    print(f"portable untracked files: {len(audit.portable_untracked)}")
    print(f"generated changes excluded: {len(audit.generated_changes)}")
    print(f"untracked residue excluded: {len(audit.ignored_untracked)}")
    print(f"unclassified untracked files: {len(audit.unknown_untracked)}")
    print(f"missing manifest paths: {len(audit.missing_portable_paths)}")
    for group in PATCH_GROUPS:
        count = sum(
            patch_group(path) == group
            for path in (*audit.portable_tracked, *audit.portable_untracked)
        )
        print(f"  {group}: {count} files")


def finalize(target_path: Path) -> None:
    target = repository_root(target_path)
    load_manifest(target)
    commands = (
        (["just", "write-app-server-schema"], target),
        (["just", "write-app-server-schema", "--experimental"], target),
        (
            [
                "cargo",
                "metadata",
                "--manifest-path",
                "Cargo.toml",
                "--format-version",
                "1",
                "--no-deps",
            ],
            target / "codex-rs",
        ),
        (["just", "bazel-lock-update"], target),
        (["just", "fmt"], target),
    )
    for command, cwd in commands:
        print(f"+ ({cwd}) {' '.join(command)}", flush=True)
        run(command, cwd=cwd, capture_output=False)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(add_help=False)
    subparsers = result.add_subparsers(dest="operation", required=True)

    audit = subparsers.add_parser("audit", add_help=False)
    audit.add_argument("--source", type=Path, default=default_source())

    export = subparsers.add_parser("export", add_help=False)
    export.add_argument("--source", type=Path, default=default_source())
    export.add_argument("--bundle", type=Path, required=True)

    check = subparsers.add_parser("check", add_help=False)
    check.add_argument("--bundle", type=Path, required=True)
    check.add_argument("--target", type=Path, required=True)

    apply = subparsers.add_parser("apply", add_help=False)
    apply.add_argument("--bundle", type=Path, required=True)
    apply.add_argument("--target", type=Path, required=True)

    port = subparsers.add_parser("port", add_help=False)
    port.add_argument("--source", type=Path, default=default_source())
    port.add_argument("--target", type=Path, required=True)

    finalize_parser = subparsers.add_parser("finalize", add_help=False)
    finalize_parser.add_argument("--target", type=Path, required=True)
    return result


def main(argv: list[str]) -> int:
    if not argv or "-h" in argv or "--help" in argv:
        print(HELP_TEXT, end="")
        return 0
    args = parser().parse_args(argv)

    if args.operation == "audit":
        source = repository_root(args.source)
        _, audit = audit_repository(source)
        print_audit(source, audit)
        audit.ensure_portable()
    elif args.operation == "export":
        bundle = export_bundle(args.source, args.bundle)
        print(f"exported customization bundle: {bundle}")
    elif args.operation == "check":
        preflight = preflight_bundle(args.bundle, args.target)
        mode = "three-way" if preflight.use_three_way else "direct"
        print(
            f"preflight passed for {len(preflight.patch_paths)} patches "
            f"against {preflight.target} ({mode})"
        )
    elif args.operation == "apply":
        target = apply_bundle(args.bundle, args.target)
        print(f"applied customization to {target}")
        print("run finalize before building the target")
    elif args.operation == "port":
        with tempfile.TemporaryDirectory(prefix="codex-local-port-") as temporary:
            bundle = export_bundle(args.source, Path(temporary))
            target = apply_bundle(bundle, args.target)
        print(f"ported customization to {target}")
        print("run finalize before building the target")
    elif args.operation == "finalize":
        finalize(args.target)
        print("generated artifacts and formatting are up to date")
    else:
        raise PortError(f"unsupported operation: {args.operation}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except PortError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
