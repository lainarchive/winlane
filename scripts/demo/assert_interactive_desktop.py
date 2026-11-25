#!/usr/bin/env python3
"""Require concrete interactive desktop evidence in a winlane UI tree."""

import json
import sys


def walk(node):
    yield node
    for child in node.get("children") or []:
        if isinstance(child, dict):
            yield from walk(child)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: assert_interactive_desktop.py UI_TREE_JSON")
    with open(sys.argv[1], encoding="utf-8-sig") as source:
        tree = json.load(source)
    if not tree.get("ok") or not isinstance(tree.get("root"), dict):
        raise SystemExit("UI tree unavailable")
    nodes = list(walk(tree["root"]))
    start = next(
        (
            node
            for node in nodes
            if str(node.get("name") or "").strip().lower() == "start"
            and node.get("controlType") == "Button"
        ),
        None,
    )
    if start is None:
        raise SystemExit("interactive desktop Start-button evidence absent")
    print(f"interactive desktop ready: start={start.get('controlType')}")


if __name__ == "__main__":
    main()
