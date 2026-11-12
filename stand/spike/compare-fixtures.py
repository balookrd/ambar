#!/usr/bin/env python3
"""Compare two `drbdsetup status --json` fixtures field by field.

Each fixture is flattened to a "path -> value" dictionary; connections are
keyed by peer name, devices and peer devices by index. Prints the keys present
in only one file and every key whose value differs.

usage: compare-fixtures.py <a.json> <b.json>
"""
import json
import sys


def flatten(node, prefix, out):
    if isinstance(node, dict):
        for k, v in node.items():
            flatten(v, f"{prefix}.{k}" if prefix else k, out)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            key = v.get("name") if prefix.endswith("connections") and isinstance(v, dict) else i
            flatten(v, f"{prefix}[{key}]", out)
    else:
        out[prefix] = node


def load(path):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    resource = data[0] if isinstance(data, list) else data
    out = {}
    flatten(resource, "", out)
    return out


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    a_path, b_path = sys.argv[1], sys.argv[2]
    a, b = load(a_path), load(b_path)
    print(f"=== {a_path.split('/')[-1]} vs {b_path.split('/')[-1]}")
    print("keys only in A", sorted(set(a) - set(b)))
    print("keys only in B", sorted(set(b) - set(a)))
    for k in sorted(set(a) & set(b)):
        if a[k] != b[k]:
            print(f"  {k}: A={a[k]!r} B={b[k]!r}")


main()
