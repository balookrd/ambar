# Code index

`scripts/code-index.py` builds a local structural index of the Rust codebase
for navigation with a small token budget. It records Cargo packages, targets,
source files, Rust items (`fn`, `struct`, `enum`, `trait`, `union`, `type`,
`const`, `static`, `mod`, `macro_rules`) and `impl` headers.

By default (`--parser auto`), the script extracts declarations from the Rust AST
via **`ast-grep`** (`ast-grep scan --inline-rules`) and automatically falls back
to the built-in regex parser when `ast-grep` is not on `PATH`. It needs
`python3` and `cargo` on `PATH` (it reads the package list from
`cargo metadata --no-deps`), and no extra Python packages.

The index is a map, not a replacement for the sources: use it to find the
relevant module, symbol and `line`..`end_line` range, then open only the source
slices the change needs.

## Generate

Run from the repository root:

```bash
python3 scripts/code-index.py
```

Outputs:

- `.code-index/repo-map.md` — compact Markdown map for humans and agents.
- `.code-index/symbols.json` — machine-readable package/file/symbol data.

`.code-index/` is ignored by git, so the generated index stays local and survives `cargo clean`.

## Options

| Flag | Default | Effect |
|---|---|---|
| `--package <name>` | all packages | limit the index to one package; repeat for several |
| `--parser <mode>` | `auto` | parser backend: `auto` (`ast-grep` with fallback to `regex`), `ast-grep` (require AST), or `regex` |
| `--self-test` | off | run built-in parser self-tests in a temporary directory without writing output |
| `--output-dir <dir>` | `.code-index` | where to write `repo-map.md` and `symbols.json`; a relative path is resolved against `--repo-root` |
| `--repo-root <dir>` | `.` (current directory) | repository root |

## Query examples

Find where a symbol is declared:

```bash
rg 'should_cleanup_client|TaintTracker|StandaloneTracker' .code-index/repo-map.md
```

Inspect the JSON with `jq` (including `line` and `end_line` ranges for slice viewing):

```bash
jq '.packages[] | select(.name == "ambar") | .source_files[] | select(.path | contains("agent/")) | {path, module, symbols: [.symbols[] | {name, line, end_line}]}' .code-index/symbols.json
```

Locate a specific symbol's exact line span:

```bash
jq '.packages[] | .source_files[] | .path as $path | .symbols[] | select(.name == "TaintTracker") | {path: $path, kind, name, line, end_line}' .code-index/symbols.json
```

`symbols.json` includes a top-level `parser` field (`"ast-grep"` or `"regex"`).
Each symbol has `kind`, `name`, `line`, `end_line`, `visibility`, `module` and
`signature`, and each `impl` entry has `line`, `end_line`, `header` and
`module`. With `ast-grep`, `end_line` holds the closing line of the item or
`impl` block; with `regex`, `end_line` is `null`.

## AST search and refactoring (`ast-grep`)

The repository configures `ast-grep` in [`sgconfig.yml`](../sgconfig.yml)
(`.ast-grep/{rules,tests,utils}`) and connects the `ast-grep-mcp` server in
[`.mcp.json`](../.mcp.json) so agents can run structural queries (`find_code`,
`find_code_by_rule`, `rewrite_code`, `dump_syntax_tree`, `test_match_code_rule`).

CLI examples for `ambar`:

```bash
# Find structs matching a pattern
ast-grep run -l rust -p 'pub struct $NAME' src/

# Find a declaration regardless of pub / pub(crate) / async modifiers
ast-grep scan --inline-rules '
id: find-taint-tracker
language: rust
rule:
  kind: struct_item
  has:
    field: name
    regex: ^TaintTracker$
' src/

# Dry-run structural rewrite (omit -U to inspect diff first; add -U to apply)
ast-grep run -l rust -p '$OLD_EXPR' -r '$NEW_EXPR' src/
```

Notes:

- Always invoke the `ast-grep` binary explicitly (not `sg`, which collides with
  `/usr/bin/sg` from `shadow-utils` on Linux).
- `ast-grep run` exits with code `1` when zero matches are found, whereas
  `ast-grep scan --json=compact` exits with `0` and prints `[]`.
- For type information, name resolution and compiler diagnostics use
  `rust-analyzer`; neither the code index nor `ast-grep` resolves types across
  crates.
