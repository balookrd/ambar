#!/usr/bin/env python3
"""Manage and validate the ambar CalVer version across Cargo.toml and Cargo.lock."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import shutil
import sys
import tempfile

CALVER_RE = re.compile(r"[0-9]{2,}\.(?:[1-9]|1[0-2])\.[0-9]+")
PACKAGE_SECTION_RE = re.compile(r"(?ms)^(\[package\]\n)(.*?)(?=^\[|\Z)")
CARGO_VERSION_RE = re.compile(r'(?m)^(version\s*=\s*")([^"]+)(")$')
LOCK_PACKAGE_RE = re.compile(r'(\[\[package\]\]\nname = "ambar"\nversion = ")([^"]+)(")')

DEFAULT_ROOT = Path(__file__).resolve().parents[2]


def validate_calver(version: str) -> None:
    """Ensure the version string matches YY.M.PATCH without leading zero in month."""
    if not CALVER_RE.fullmatch(version):
        raise SystemExit(
            f"invalid CalVer version '{version}': expected YY.M.PATCH without leading zero in month (e.g. 26.11.0)"
        )


def read_versions(root: Path) -> tuple[str, str]:
    """Return (cargo_toml_version, cargo_lock_version) for package 'ambar'."""
    cargo_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"

    cargo_text = cargo_path.read_text(encoding="utf-8")
    sec_match = PACKAGE_SECTION_RE.search(cargo_text)
    if not sec_match:
        raise SystemExit(f"failed to find [package] section in {cargo_path}")
    ver_matches = list(CARGO_VERSION_RE.finditer(sec_match.group(2)))
    if len(ver_matches) != 1:
        raise SystemExit(
            f"expected exactly 1 version field in [package] of {cargo_path}, found {len(ver_matches)}"
        )
    cargo_version = ver_matches[0].group(2)

    lock_text = lock_path.read_text(encoding="utf-8")
    lock_matches = list(LOCK_PACKAGE_RE.finditer(lock_text))
    if len(lock_matches) != 1:
        raise SystemExit(
            f"expected exactly 1 'ambar' package entry in {lock_path}, found {len(lock_matches)}"
        )
    lock_version = lock_matches[0].group(2)

    return cargo_version, lock_version


def cmd_get(root: Path) -> int:
    cargo_version, _ = read_versions(root)
    print(cargo_version)
    return 0


def cmd_set(root: Path, version: str) -> int:
    validate_calver(version)

    cargo_path = root / "Cargo.toml"
    cargo_text = cargo_path.read_text(encoding="utf-8")
    sec_match = PACKAGE_SECTION_RE.search(cargo_text)
    if not sec_match:
        raise SystemExit(f"failed to find [package] section in {cargo_path}")
    sec_body, count = CARGO_VERSION_RE.subn(
        rf"\g<1>{version}\g<3>",
        sec_match.group(2),
        count=1,
    )
    if count != 1:
        raise SystemExit(f"failed to update [package].version in {cargo_path}")
    new_cargo = cargo_text[: sec_match.start(2)] + sec_body + cargo_text[sec_match.end(2) :]
    cargo_path.write_text(new_cargo, encoding="utf-8")

    lock_path = root / "Cargo.lock"
    lock_text = lock_path.read_text(encoding="utf-8")
    new_lock, lock_count = LOCK_PACKAGE_RE.subn(
        rf"\g<1>{version}\g<3>",
        lock_text,
        count=1,
    )
    if lock_count != 1:
        raise SystemExit(f"failed to update 'ambar' version in {lock_path}")
    lock_path.write_text(new_lock, encoding="utf-8")
    return 0


def cmd_check(root: Path, expected: str) -> int:
    validate_calver(expected)
    cargo_version, lock_version = read_versions(root)
    if cargo_version != expected or lock_version != expected:
        raise SystemExit(
            f"version mismatch for expected '{expected}': Cargo.toml='{cargo_version}', Cargo.lock='{lock_version}'"
        )
    return 0


def expect_exit(fn, *args) -> None:
    try:
        fn(*args)
    except SystemExit:
        return
    raise AssertionError(f"expected SystemExit for {fn.__name__}{args}")


def cmd_self_test(root: Path) -> int:
    cargo_version, lock_version = read_versions(root)
    if cargo_version != lock_version:
        raise SystemExit(
            f"Cargo.toml ({cargo_version}) and Cargo.lock ({lock_version}) are out of sync"
        )

    with tempfile.TemporaryDirectory() as tmp:
        tmp_root = Path(tmp)
        shutil.copy2(root / "Cargo.toml", tmp_root / "Cargo.toml")
        shutil.copy2(root / "Cargo.lock", tmp_root / "Cargo.lock")

        cmd_set(tmp_root, "26.11.0")
        cmd_check(tmp_root, "26.11.0")
        got_cargo, got_lock = read_versions(tmp_root)
        assert got_cargo == "26.11.0", f"expected 26.11.0, got {got_cargo}"
        assert got_lock == "26.11.0", f"expected 26.11.0, got {got_lock}"

        for bad in ("26.01.0", "26.13.0", "1.2.3", "v26.11.0", "26.11.0\n"):
            expect_exit(validate_calver, bad)
            expect_exit(cmd_set, tmp_root, bad)

        expect_exit(cmd_check, tmp_root, "26.11.1")

        # Verify no unrelated lines in Cargo.toml or Cargo.lock changed.
        orig_cargo_lines = (root / "Cargo.toml").read_text(encoding="utf-8").splitlines()
        new_cargo_lines = (tmp_root / "Cargo.toml").read_text(encoding="utf-8").splitlines()
        diffs = [i for i, (a, b) in enumerate(zip(orig_cargo_lines, new_cargo_lines), 1) if a != b]
        assert len(orig_cargo_lines) == len(new_cargo_lines) and diffs == [3], (
            f"unexpected Cargo.toml diff lines: {diffs}"
        )

        orig_lock_lines = (root / "Cargo.lock").read_text(encoding="utf-8").splitlines()
        new_lock_lines = (tmp_root / "Cargo.lock").read_text(encoding="utf-8").splitlines()
        lock_diffs = [i for i, (a, b) in enumerate(zip(orig_lock_lines, new_lock_lines), 1) if a != b]
        assert len(orig_lock_lines) == len(new_lock_lines) and len(lock_diffs) == 1, (
            f"unexpected Cargo.lock diff lines: {lock_diffs}"
        )

    print("OK: version.py self-test passed")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=DEFAULT_ROOT,
        help="Repository root containing Cargo.toml and Cargo.lock",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("get", help="Print package version from Cargo.toml")

    p_set = sub.add_parser("set", help="Set CalVer version in Cargo.toml and Cargo.lock")
    p_set.add_argument("version", help="CalVer version (YY.M.PATCH)")

    p_check = sub.add_parser("check", help="Verify Cargo.toml and Cargo.lock match expected CalVer")
    p_check.add_argument("version", help="Expected CalVer version (YY.M.PATCH)")

    sub.add_parser("self-test", help="Run built-in verification against a temporary copy")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    root = args.root.resolve()
    if args.command == "get":
        return cmd_get(root)
    if args.command == "set":
        return cmd_set(root, args.version)
    if args.command == "check":
        return cmd_check(root, args.version)
    if args.command == "self-test":
        return cmd_self_test(root)
    return 2


if __name__ == "__main__":
    sys.exit(main())
