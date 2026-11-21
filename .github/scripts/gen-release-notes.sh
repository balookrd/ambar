#!/usr/bin/env bash
# Generate a GitHub release body for ambar from Conventional Commits.
#
#   gen-release-notes.sh <tag> <output-file> [end-ref]
#     tag         : CalVer tag vYY.M.PATCH (e.g. v26.11.0)
#     output-file : path to write the markdown body to
#     end-ref     : optional git ref for the end of the range (defaults to <tag>;
#                   pass HEAD for a local dry-run before the tag exists)
#
# Requires git-cliff on PATH and a checkout with full history + tags
# (fetch-depth: 0, fetch-tags: true). Compatible with bash 3.2 (macOS).
set -euo pipefail

tag="${1:?usage: gen-release-notes.sh <tag> <output-file> [end-ref]}"
out="${2:?usage: gen-release-notes.sh <tag> <output-file> [end-ref]}"
end_ref="${3:-$tag}"

if [[ ! "$tag" =~ ^v[0-9]{2,}\.([1-9]|1[0-2])\.[0-9]+$ ]]; then
  echo "expected CalVer tag vYY.M.PATCH without leading zero in month, got: $tag" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
config="${repo_root}/cliff.toml"

# Find the previous CalVer tag strictly less than <tag> using version sort.
# Avoid `mapfile` for bash 3.2 compatibility on macOS.
calver_tags="$(git -C "$repo_root" tag -l 'v[0-9][0-9].*' | grep -E '^v[0-9]{2,}\.([1-9]|1[0-2])\.[0-9]+$' || true)"
prev="$( { printf '%s\n' "$calver_tags" | grep -vx "$tag" || true; printf '%s\n' "$tag"; } \
          | grep -v '^$' | sort -V | awk -v t="$tag" '$0 == t { print p; exit } { p = $0 }')"

if [[ -n "$prev" ]]; then
  range="${prev}..${end_ref}"
  compare_from="$prev"
else
  root="$(git -C "$repo_root" rev-list --max-parents=0 "$end_ref" 2>/dev/null | tail -1)"
  if [[ -z "$root" ]]; then
    echo "failed to resolve root commit for ref '$end_ref'" >&2
    exit 1
  fi
  range="${root}..${end_ref}"
  compare_from="${root:0:8}"
fi

if [[ "$end_ref" == "$tag" ]]; then
  compare_to="$tag"
else
  compare_to="$(git -C "$repo_root" rev-parse --short "$end_ref")"
fi

# git-cliff exits non-zero when the range has no releasable commits; capture and
# fall through to the fallback body instead of failing the release.
body=""
if body="$(git -C "$repo_root" cliff "$range" \
      --tag-pattern '^v[0-9]{2,}\.' \
      --tag "$tag" \
      --config "$config" \
      --strip all 2>/dev/null)"; then
  :
else
  body=""
fi

server_url="${GITHUB_SERVER_URL:-https://github.com}"
repo="${GITHUB_REPOSITORY:-}"
if [[ -z "$repo" ]]; then
  origin="$(git -C "$repo_root" remote get-url origin 2>/dev/null || echo '')"
  repo="$(printf '%s' "$origin" | sed -E 's#^.*[:/]([^/]+/[^/]+)$#\1#; s#\.git$##')"
fi

footer=""
if [[ -n "$compare_from" && -n "$repo" ]]; then
  footer="**Full Changelog**: ${server_url}/${repo}/compare/${compare_from}...${compare_to}"
fi

# Fallback when nothing survived filtering (detect emptiness by lack of items).
if ! printf '%s' "$body" | grep -q '^- '; then
  body="## What's Changed in ${tag}

_No user-facing changes in this release; see the full changelog below._"
fi

{
  printf '%s\n' "$body"
  if [[ -n "$footer" ]]; then
    printf '\n%s\n' "$footer"
  fi
} > "$out"

echo "Wrote release notes to $out ($(wc -l < "$out") lines)"
