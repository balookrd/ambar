# Code index

`scripts/code-index.py` builds a local structural index of the Rust codebase
for navigation with a small token budget. It records Cargo packages, targets,
source files, top-level Rust items and `impl` headers. It needs `python3` and
`cargo` on `PATH` (it reads the package list from `cargo metadata --no-deps`)
and no extra Python packages.

The index is a map, not a replacement for the sources: use it to find the
relevant module and symbol, then open only the source ranges the change needs.

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
| `--output-dir <dir>` | `.code-index` | where to write `repo-map.md` and `symbols.json`; a relative path is resolved against `--repo-root` |
| `--repo-root <dir>` | `.` (current directory) | repository root |

## Query examples

Find where a symbol is declared:

```bash
rg 'should_cleanup_client|VolumeQuorumCheck|StandaloneTracker' .code-index/repo-map.md
```

Inspect the JSON with `jq`:

```bash
jq '.packages[] | select(.name == "ambar") | .source_files[] | select(.path | contains("agent/")) | {path, module, symbols: [.symbols[].name]}' .code-index/symbols.json
```

Each symbol in `symbols.json` has `kind`, `name`, `line`, `visibility`, `module`
and `signature`. For type information, references and diagnostics use
`rust-analyzer`; the index does not resolve names.
