#!/usr/bin/env python3
"""Build a compact Rust codebase index for low-token navigation.

The index is intentionally structural rather than exhaustive: it records Cargo
packages, targets, source files, top-level/member Rust items, and impl headers
into `.code-index/` (`repo-map.md` and `symbols.json`). By default it extracts
symbols from the Rust AST via `ast-grep` and falls back to the built-in regex
parser if `ast-grep` is unavailable on `PATH`.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


DEFAULT_OUTPUT_DIR = Path(".code-index")
AST_GREP_BATCH_SIZE = 200

VIS_RE = re.compile(r"^\s*(pub(?:\s*\([^)]*\)|\b))")

ITEM_RE = re.compile(
    r"""
    ^\s*
    (?P<vis>pub(?:\s*\([^)]*\))?)?\s*
    (?:(?:async|unsafe|const)\s+)*
    (?P<kind>fn|struct|enum|trait|union|type|const|static|mod)\s+
    (?P<name>[A-Za-z_][A-Za-z0-9_]*)
    """,
    re.VERBOSE,
)
MACRO_RULES_RE = re.compile(
    r"^\s*(?P<vis>pub(?:\s*\([^)]*\))?)?\s*macro_rules!\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
)
IMPL_RE = re.compile(r"^\s*(?P<unsafe>unsafe\s+)?impl(?:\s*<[^{};]*>)?\s+(?P<header>[^{};]+?)(?:\s*\{|$)")

AST_GREP_RUST_RULES = """\
id: fn
language: rust
rule:
  kind: function_item
  has:
    field: name
    pattern: $NAME
---
id: fn_sig
language: rust
rule:
  kind: function_signature_item
  has:
    field: name
    pattern: $NAME
---
id: struct
language: rust
rule:
  kind: struct_item
  has:
    field: name
    pattern: $NAME
---
id: enum
language: rust
rule:
  kind: enum_item
  has:
    field: name
    pattern: $NAME
---
id: trait
language: rust
rule:
  kind: trait_item
  has:
    field: name
    pattern: $NAME
---
id: union
language: rust
rule:
  kind: union_item
  has:
    field: name
    pattern: $NAME
---
id: type
language: rust
rule:
  any:
    - kind: type_item
    - kind: associated_type
  has:
    field: name
    pattern: $NAME
---
id: const
language: rust
rule:
  kind: const_item
  has:
    field: name
    pattern: $NAME
---
id: static
language: rust
rule:
  kind: static_item
  has:
    field: name
    pattern: $NAME
---
id: mod
language: rust
rule:
  kind: mod_item
  has:
    field: name
    pattern: $NAME
---
id: macro_rules
language: rust
rule:
  kind: macro_definition
  has:
    field: name
    pattern: $NAME
---
id: impl
language: rust
rule:
  kind: impl_item
"""


@dataclass(frozen=True)
class Target:
    name: str
    kind: list[str]
    crate_types: list[str]
    src_path: str


@dataclass(frozen=True)
class Symbol:
    kind: str
    name: str
    line: int
    end_line: int | None
    visibility: str
    module: str
    signature: str


@dataclass(frozen=True)
class ImplBlock:
    line: int
    end_line: int | None
    header: str
    module: str


@dataclass(frozen=True)
class SourceFile:
    path: str
    module: str
    line_count: int
    symbols: list[Symbol]
    impls: list[ImplBlock]


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    workspace: str
    manifest_path: str
    package_root: str
    targets: list[Target]
    features: list[str]
    source_files: list[SourceFile]


def run_json(command: list[str], cwd: Path) -> dict[str, Any]:
    proc = subprocess.run(command, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if proc.returncode != 0:
        raise RuntimeError(f"{' '.join(command)} failed:\n{proc.stderr.strip()}")
    return json.loads(proc.stdout)


def cargo_metadata(workspace_path: Path) -> dict[str, Any]:
    return run_json(["cargo", "metadata", "--format-version", "1", "--no-deps"], workspace_path)


def relative_to_root(path: Path, root: Path) -> str:
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return path.resolve().as_posix()


def resolve_parser(requested: str) -> tuple[str, str | None]:
    # Never fall back to `sg`: on Linux `/usr/bin/sg` is shadow-utils' newgrp alias.
    bin_path = shutil.which("ast-grep")
    if requested == "regex":
        return ("regex", None)
    if requested == "ast-grep":
        if not bin_path:
            raise RuntimeError("ast-grep binary not found on PATH (install ast-grep or pass --parser regex)")
        return ("ast-grep", bin_path)
    if requested == "auto":
        if bin_path:
            return ("ast-grep", bin_path)
        print("warning: ast-grep not found on PATH, falling back to regex parser", file=sys.stderr)
        return ("regex", None)
    raise RuntimeError(f"unsupported parser: {requested}")


def mask_comments(source: str) -> str:
    """Replace Rust comments with spaces while preserving byte-ish layout and lines."""
    out: list[str] = []
    i = 0
    block_depth = 0
    in_string = False
    in_char = False
    raw_hashes: int | None = None

    while i < len(source):
        ch = source[i]
        nxt = source[i + 1] if i + 1 < len(source) else ""

        if block_depth:
            if ch == "/" and nxt == "*":
                block_depth += 1
                out.extend("  ")
                i += 2
                continue
            if ch == "*" and nxt == "/":
                block_depth -= 1
                out.extend("  ")
                i += 2
                continue
            out.append("\n" if ch == "\n" else " ")
            i += 1
            continue

        if raw_hashes is not None:
            out.append(ch)
            if ch == '"':
                hashes = source[i + 1 : i + 1 + raw_hashes]
                if hashes == "#" * raw_hashes:
                    out.extend(hashes)
                    i += raw_hashes + 1
                    raw_hashes = None
                    continue
            i += 1
            continue

        if in_string:
            out.append(ch)
            if ch == "\\":
                if i + 1 < len(source):
                    out.append(source[i + 1])
                    i += 2
                    continue
            elif ch == '"':
                in_string = False
            i += 1
            continue

        if in_char:
            out.append(ch)
            if ch == "\\":
                if i + 1 < len(source):
                    out.append(source[i + 1])
                    i += 2
                    continue
            elif ch == "'":
                in_char = False
            i += 1
            continue

        if ch == "/" and nxt == "/":
            out.extend("  ")
            i += 2
            while i < len(source) and source[i] != "\n":
                out.append(" ")
                i += 1
            continue

        if ch == "/" and nxt == "*":
            block_depth = 1
            out.extend("  ")
            i += 2
            continue

        if ch == "r":
            raw = re.match(r'r(#+)"', source[i:])
            if raw:
                raw_hashes = len(raw.group(1))
                token = raw.group(0)
                out.append(token)
                i += len(token)
                continue
            if source.startswith('r"', i):
                raw_hashes = 0
                out.append('r"')
                i += 2
                continue

        if ch == '"':
            in_string = True
        elif ch == "'":
            in_char = True
        out.append(ch)
        i += 1

    return "".join(out)


def rust_module_path(file_path: Path, package_root: Path) -> str:
    resolved_file = file_path.resolve()
    resolved_root = package_root.resolve()
    src_dir = resolved_root / "src"
    try:
        rel = resolved_file.relative_to(src_dir)
    except ValueError:
        rel = resolved_file.relative_to(resolved_root)

    parts = list(rel.with_suffix("").parts)
    if parts in (["lib"], ["main"]):
        return "crate"
    if parts[-1] == "mod":
        parts = parts[:-1]
    return "::".join(parts) if parts else "crate"


def clean_signature(line: str) -> str:
    return re.sub(r"\s+", " ", line.strip())[:180]


def parse_rust_file_regex(file_path: Path, package_root: Path, repo_root: Path) -> SourceFile:
    source = file_path.read_text(encoding="utf-8", errors="replace")
    masked = mask_comments(source)
    original_lines = source.splitlines()
    masked_lines = masked.splitlines()
    module = rust_module_path(file_path, package_root)

    symbols: list[Symbol] = []
    impls: list[ImplBlock] = []

    for line_no, masked_line in enumerate(masked_lines, start=1):
        original_line = original_lines[line_no - 1] if line_no - 1 < len(original_lines) else masked_line
        item = ITEM_RE.match(masked_line)
        if item:
            visibility = item.group("vis") or ""
            symbols.append(
                Symbol(
                    kind=item.group("kind"),
                    name=item.group("name"),
                    line=line_no,
                    end_line=None,
                    visibility=visibility.strip(),
                    module=module,
                    signature=clean_signature(original_line),
                )
            )
            continue

        macro = MACRO_RULES_RE.match(masked_line)
        if macro:
            visibility = macro.group("vis") or ""
            symbols.append(
                Symbol(
                    kind="macro_rules",
                    name=macro.group("name"),
                    line=line_no,
                    end_line=None,
                    visibility=visibility.strip(),
                    module=module,
                    signature=clean_signature(original_line),
                )
            )
            continue

        impl = IMPL_RE.match(masked_line)
        if impl:
            header = clean_signature(original_line)
            if header.startswith("impl"):
                impls.append(ImplBlock(line=line_no, end_line=None, header=header[:220], module=module))

    return SourceFile(
        path=relative_to_root(file_path, repo_root),
        module=module,
        line_count=len(original_lines),
        symbols=symbols,
        impls=impls,
    )


def parse_rust_files_ast(
    file_paths: list[Path],
    package_root: Path,
    repo_root: Path,
    ast_grep_bin: str,
) -> list[SourceFile]:
    if not file_paths:
        return []

    resolved_root = repo_root.resolve()
    resolved_paths = [path.resolve() for path in file_paths]
    matches_by_file: dict[Path, list[dict[str, Any]]] = {path: [] for path in resolved_paths}

    for start in range(0, len(resolved_paths), AST_GREP_BATCH_SIZE):
        batch = resolved_paths[start : start + AST_GREP_BATCH_SIZE]
        proc = subprocess.run(
            [
                ast_grep_bin,
                "scan",
                "--inline-rules",
                AST_GREP_RUST_RULES,
                "--json=compact",
                *(str(path) for path in batch),
            ],
            cwd=resolved_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if proc.returncode != 0:
            raise RuntimeError(f"ast-grep scan failed:\n{proc.stderr.strip()}")
        raw_matches = json.loads(proc.stdout) if proc.stdout.strip() else []
        for match in raw_matches:
            match_path = Path(match["file"])
            if not match_path.is_absolute():
                match_path = resolved_root / match_path
            file_key = match_path.resolve()
            if file_key in matches_by_file:
                matches_by_file[file_key].append(match)

    results: list[SourceFile] = []
    for file_path in resolved_paths:
        source = file_path.read_text(encoding="utf-8", errors="replace")
        original_lines = source.splitlines()
        module = rust_module_path(file_path, package_root)

        symbols: list[Symbol] = []
        impls: list[ImplBlock] = []

        for match in matches_by_file[file_path]:
            start_line = match["range"]["start"]["line"] + 1
            end_line = match["range"]["end"]["line"] + 1
            rule_id = match["ruleId"]
            text = match.get("text", "")

            if rule_id == "impl":
                head_part = text.split("{", 1)[0].strip()
                header = clean_signature(head_part) + (" {" if "{" in text else "")
                impls.append(ImplBlock(line=start_line, end_line=end_line, header=header[:220], module=module))
                continue

            kind = "fn" if rule_id == "fn_sig" else rule_id
            name = match["metaVariables"]["single"]["NAME"]["text"]
            vis_match = VIS_RE.match(text)
            visibility = vis_match.group(1).strip() if vis_match else ""
            sig_line = (
                original_lines[start_line - 1]
                if 0 <= start_line - 1 < len(original_lines)
                else (text.splitlines()[0] if text else "")
            )
            symbols.append(
                Symbol(
                    kind=kind,
                    name=name,
                    line=start_line,
                    end_line=end_line,
                    visibility=visibility,
                    module=module,
                    signature=clean_signature(sig_line),
                )
            )

        # `ast-grep scan` traverses files in parallel; sort items for deterministic output.
        symbols.sort(key=lambda symbol: (symbol.line, symbol.end_line or symbol.line, symbol.kind, symbol.name))
        impls.sort(key=lambda block: (block.line, block.end_line or block.line, block.header))

        results.append(
            SourceFile(
                path=relative_to_root(file_path, resolved_root),
                module=module,
                line_count=len(original_lines),
                symbols=symbols,
                impls=impls,
            )
        )

    return results


def collect_source_files(package_root: Path, targets: Iterable[Target], repo_root: Path) -> list[Path]:
    files: set[Path] = set()
    src_dir = package_root / "src"
    if src_dir.exists():
        files.update(path for path in src_dir.rglob("*.rs") if path.is_file())
    for target in targets:
        src = Path(target.src_path)
        if not src.is_absolute():
            src = repo_root / src
        if src.exists() and src.suffix == ".rs":
            files.add(src)
    return sorted(files)


def package_from_metadata(
    package: dict[str, Any],
    workspace_name: str,
    repo_root: Path,
    package_filter: set[str],
    parser_name: str,
    ast_grep_bin: str | None,
) -> Package | None:
    manifest = Path(package["manifest_path"])
    package_root = manifest.parent
    if package_filter and package["name"] not in package_filter:
        return None

    targets = [
        Target(
            name=target["name"],
            kind=list(target.get("kind", [])),
            crate_types=list(target.get("crate_types", [])),
            src_path=relative_to_root(Path(target["src_path"]), repo_root),
        )
        for target in package.get("targets", [])
    ]

    files = collect_source_files(package_root, targets, repo_root)
    if parser_name == "ast-grep":
        assert ast_grep_bin is not None
        source_files = parse_rust_files_ast(files, package_root, repo_root, ast_grep_bin)
    else:
        source_files = [parse_rust_file_regex(path, package_root, repo_root) for path in files]

    return Package(
        name=package["name"],
        version=package["version"],
        workspace=workspace_name,
        manifest_path=relative_to_root(manifest, repo_root),
        package_root=relative_to_root(package_root, repo_root),
        targets=targets,
        features=sorted(package.get("features", {}).keys()),
        source_files=source_files,
    )


def load_workspace_packages(
    workspace_path: Path,
    workspace_name: str,
    repo_root: Path,
    package_filter: set[str],
    parser_name: str,
    ast_grep_bin: str | None,
) -> list[Package]:
    metadata = cargo_metadata(workspace_path)
    workspace_members = set(metadata.get("workspace_members", []))
    packages: list[Package] = []
    for package in metadata.get("packages", []):
        if package["id"] not in workspace_members:
            continue
        parsed = package_from_metadata(
            package,
            workspace_name,
            repo_root,
            package_filter,
            parser_name,
            ast_grep_bin,
        )
        if parsed is not None:
            packages.append(parsed)
    return packages


def package_symbol_count(package: Package) -> int:
    return sum(len(source.symbols) for source in package.source_files)


def package_impl_count(package: Package) -> int:
    return sum(len(source.impls) for source in package.source_files)


def as_json(data: dict[str, Any]) -> str:
    return json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def markdown_table_row(cells: Iterable[Any]) -> str:
    rendered = [str(cell).replace("\n", " ").replace("|", r"\|") for cell in cells]
    return "| " + " | ".join(rendered) + " |"


def format_target(target: Target) -> str:
    kinds = ",".join(target.kind) if target.kind else "-"
    return f"{target.name} ({kinds})"


def write_markdown(data: dict[str, Any]) -> str:
    lines: list[str] = []
    lines.append("# Code Index")
    lines.append("")
    lines.append("Generated for low-token navigation. Use this as a map, then open only the files and line ranges relevant to the task.")
    lines.append("")
    lines.append(f"- Generated at: `{data['generated_at']}`")
    lines.append(f"- Repository root: `{data['repo_root']}`")
    lines.append(f"- Parser: `{data['parser']}`")
    lines.append(f"- Packages: `{data['totals']['packages']}`")
    lines.append(f"- Rust files: `{data['totals']['rust_files']}`")
    lines.append(f"- Symbols: `{data['totals']['symbols']}`")
    lines.append(f"- Impl blocks: `{data['totals']['impl_blocks']}`")
    lines.append("")
    lines.append("## Packages")
    lines.append("")
    lines.append(markdown_table_row(["Package", "Workspace", "Path", "Files", "Symbols", "Impls", "Targets"]))
    lines.append(markdown_table_row(["---", "---", "---", "---:", "---:", "---:", "---"]))

    for package in data["packages"]:
        targets = ", ".join(format_target(Target(**target)) for target in package["targets"])
        lines.append(
            markdown_table_row(
                [
                    f"`{package['name']}`",
                    f"`{package['workspace']}`",
                    f"`{package['package_root']}`",
                    len(package["source_files"]),
                    sum(len(source["symbols"]) for source in package["source_files"]),
                    sum(len(source["impls"]) for source in package["source_files"]),
                    targets or "-",
                ]
            )
        )

    for package in data["packages"]:
        lines.append("")
        lines.append(f"## `{package['name']}`")
        lines.append("")
        lines.append(f"- Path: `{package['package_root']}`")
        lines.append(f"- Manifest: `{package['manifest_path']}`")
        if package["features"]:
            feature_list = ", ".join(f"`{feature}`" for feature in package["features"])
            lines.append(f"- Features: {feature_list}")
        lines.append("")
        lines.append(markdown_table_row(["File", "Module", "Lines", "Items", "Impls"]))
        lines.append(markdown_table_row(["---", "---", "---:", "---", "---"]))

        for source in package["source_files"]:
            item_names = ", ".join(
                f"`{symbol['kind']} {symbol['name']}`" for symbol in source["symbols"][:18]
            )
            if len(source["symbols"]) > 18:
                item_names += f", ... +{len(source['symbols']) - 18}"
            impls = ", ".join(f"`{impl['header']}`" for impl in source["impls"][:6])
            if len(source["impls"]) > 6:
                impls += f", ... +{len(source['impls']) - 6}"
            lines.append(
                markdown_table_row(
                    [
                        f"`{source['path']}`",
                        f"`{source['module']}`",
                        source["line_count"],
                        item_names or "-",
                        impls or "-",
                    ]
                )
            )

    lines.append("")
    return "\n".join(lines)


def build_index(args: argparse.Namespace) -> dict[str, Any]:
    repo_root = Path(args.repo_root).resolve()
    package_filter = set(args.package)
    parser_name, ast_grep_bin = resolve_parser(args.parser)
    packages = load_workspace_packages(
        repo_root,
        "root",
        repo_root,
        package_filter=package_filter,
        parser_name=parser_name,
        ast_grep_bin=ast_grep_bin,
    )

    packages.sort(key=lambda package: (package.workspace, package.name))
    rust_files = sum(len(package.source_files) for package in packages)
    symbols = sum(package_symbol_count(package) for package in packages)
    impls = sum(package_impl_count(package) for package in packages)

    return {
        "generated_at": datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
        "repo_root": repo_root.as_posix(),
        "parser": parser_name,
        "totals": {
            "packages": len(packages),
            "rust_files": rust_files,
            "symbols": symbols,
            "impl_blocks": impls,
        },
        "packages": [asdict(package) for package in packages],
    }


def run_self_test(requested_parser: str) -> int:
    sample_lib = """\
const RAW: &str = r#"
pub fn fake_in_string() {}
struct FakeStruct;
impl FakeImpl {
"#;

// fn commented() {}
/* /* fn commented() {} */ */

const _: () = assert!(true);

pub(crate) unsafe extern "C" fn c_entry() {}

#[inline]
/// doc comment
pub async fn multi(
    x: u32,
) -> u32 {
    x + 1
}

pub trait Sample {
    type Output;
    fn req(&self);
}

pub struct Foo<T>(T);

impl<T> Foo<T>
where
    T: Send
{
    fn bar(&self) {}
}

macro_rules! my_macro { () => {}; }

pub union RawBits { a: u32, b: f32 }

pub mod sub;
"""

    checked: list[str] = []
    with tempfile.TemporaryDirectory() as tmpdir:
        root = Path(tmpdir).resolve()
        src_dir = root / "src"
        src_dir.mkdir()
        lib_rs = src_dir / "lib.rs"
        empty_rs = src_dir / "empty.rs"
        lib_rs.write_text(sample_lib, encoding="utf-8")
        empty_rs.write_text("// only comments\n/* empty */\n", encoding="utf-8")

        regex_lib = parse_rust_file_regex(lib_rs, root, root)
        regex_empty = parse_rust_file_regex(empty_rs, root, root)
        assert not regex_empty.symbols and not regex_empty.impls, "regex matched empty file"
        assert all(s.end_line is None for s in regex_lib.symbols), "regex end_line must be None"
        assert all(b.end_line is None for b in regex_lib.impls), "regex impl end_line must be None"
        regex_names = {s.name for s in regex_lib.symbols}
        assert {"multi", "Sample", "Foo", "bar", "my_macro", "RawBits", "sub"}.issubset(regex_names), (
            f"regex missing expected items: {regex_names}"
        )
        checked.append("regex")

        if requested_parser != "regex":
            parser_name, ast_grep_bin = resolve_parser(requested_parser)
            if parser_name == "ast-grep":
                assert ast_grep_bin is not None
                ast_files = parse_rust_files_ast([lib_rs, empty_rs], root, root, ast_grep_bin)
                ast_lib, ast_empty = ast_files
                assert not ast_empty.symbols and not ast_empty.impls, "ast-grep matched empty file"

                by_name = {s.name: s for s in ast_lib.symbols}
                for forbidden in ("fake_in_string", "FakeStruct", "FakeImpl", "commented"):
                    assert forbidden not in by_name, f"ast-grep falsely matched {forbidden}"
                assert all("FakeImpl" not in b.header for b in ast_lib.impls), "ast-grep falsely matched FakeImpl"

                expected_kinds = {
                    "RAW": "const",
                    "_": "const",
                    "c_entry": "fn",
                    "multi": "fn",
                    "Sample": "trait",
                    "Output": "type",
                    "req": "fn",
                    "Foo": "struct",
                    "bar": "fn",
                    "my_macro": "macro_rules",
                    "RawBits": "union",
                    "sub": "mod",
                }
                for sym_name, sym_kind in expected_kinds.items():
                    assert sym_name in by_name, f"ast-grep missed {sym_name}"
                    assert by_name[sym_name].kind == sym_kind, f"{sym_name} kind={by_name[sym_name].kind}"

                c_entry = by_name["c_entry"]
                assert c_entry.visibility == "pub(crate)", c_entry
                assert c_entry.end_line == c_entry.line, c_entry

                multi = by_name["multi"]
                lib_lines = sample_lib.splitlines()
                assert multi.visibility == "pub", multi
                assert multi.end_line is not None and multi.end_line > multi.line, multi
                assert lib_lines[multi.line - 1].startswith("pub async fn multi("), lib_lines[multi.line - 1]

                assert by_name["Sample"].visibility == "pub"
                assert by_name["Foo"].visibility == "pub"
                assert by_name["RawBits"].visibility == "pub"
                assert by_name["sub"].visibility == "pub"

                assert len(ast_lib.impls) == 1, ast_lib.impls
                impl_block = ast_lib.impls[0]
                assert impl_block.header == "impl<T> Foo<T> where T: Send {", impl_block
                assert impl_block.end_line is not None and impl_block.end_line > impl_block.line, impl_block

                ast_files_again = parse_rust_files_ast([lib_rs, empty_rs], root, root, ast_grep_bin)
                assert ast_files_again == ast_files, "ast-grep parser output is non-deterministic"
                checked.append("ast-grep")

    print(f"OK: code-index self-test passed (checked: {', '.join(checked)})")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root. Defaults to the current directory.")
    parser.add_argument("--output-dir", default=DEFAULT_OUTPUT_DIR.as_posix(), help="Directory for repo-map.md and symbols.json.")
    parser.add_argument("--package", action="append", default=[], help="Limit the index to one package name. Can be passed more than once.")
    parser.add_argument(
        "--parser",
        choices=("auto", "ast-grep", "regex"),
        default="auto",
        help="Parser backend: ast-grep AST (default with regex fallback), ast-grep (require AST), or regex.",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run built-in parser self-tests in a temporary directory without writing output.",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        try:
            return run_self_test(args.parser)
        except (AssertionError, RuntimeError) as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 1

    repo_root = Path(args.repo_root).resolve()
    output_dir = (repo_root / args.output_dir).resolve()

    try:
        index = build_index(args)
    except RuntimeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    output_dir.mkdir(parents=True, exist_ok=True)
    json_path = output_dir / "symbols.json"
    markdown_path = output_dir / "repo-map.md"
    json_path.write_text(as_json(index), encoding="utf-8")
    markdown_path.write_text(write_markdown(index), encoding="utf-8")

    print(f"wrote {relative_to_root(markdown_path, repo_root)}")
    print(f"wrote {relative_to_root(json_path, repo_root)}")
    print(
        "indexed "
        f"{index['totals']['packages']} packages, "
        f"{index['totals']['rust_files']} Rust files, "
        f"{index['totals']['symbols']} symbols, "
        f"{index['totals']['impl_blocks']} impl blocks "
        f"(parser: {index['parser']})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
