#!/usr/bin/env python3
"""Find Notepad's first-run dismissal button in a winlane UI tree, if present."""

import json
import sys


def walk(node):
    yield node
    for child in node.get("children") or []:
        if isinstance(child, dict):
            yield from walk(child)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: find_notepad_popup_button.py UI_TREE_JSON")
    with open(sys.argv[1], encoding="utf-8-sig") as source:
        tree = json.load(source)
    if not tree.get("ok") or not isinstance(tree.get("root"), dict):
        raise SystemExit(2)
    if tree.get("truncated"):
        raise SystemExit(3)
    for node in walk(tree["root"]):
        name = str(node.get("name") or "").strip().lower()
        if node.get("controlType") == "Button" and name == "got it":
            path = node.get("id")
            if isinstance(path, str) and path:
                print(path)
                return


if __name__ == "__main__":
    main()
