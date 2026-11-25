#!/usr/bin/env python3
"""Find the first edit/document node inside a Notepad window in a winlane UI tree."""

import json
import sys


def fail(message: str) -> None:
    print(f"find-notepad-node error: {message}", file=sys.stderr)
    raise SystemExit(1)


def children(node):
    for child in node.get("children") or []:
        if isinstance(child, dict):
            yield child


def find_text_node(node):
    if node.get("controlType") in {"Document", "Edit"}:
        return node
    for child in children(node):
        found = find_text_node(child)
        if found is not None:
            return found
    return None


def find_notepad_window(node):
    name = str(node.get("name") or "").lower()
    if node.get("controlType") == "Window" and "notepad" in name:
        return node
    for child in children(node):
        found = find_notepad_window(child)
        if found is not None:
            return found
    return None


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: find_notepad_node.py UI_TREE_JSON")
    try:
        with open(sys.argv[1], encoding="utf-8-sig") as source:
            tree = json.load(source)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read UI tree: {error}")
    if not tree.get("ok"):
        fail(f"UI tree command did not succeed: {tree.get('error', 'unknown error')}")
    root = tree.get("root")
    if not isinstance(root, dict):
        fail("UI tree has no root object")
    window = find_notepad_window(root)
    if window is None:
        fail("no Notepad window found in UI tree")
    node = find_text_node(window)
    if node is None:
        fail("no Document or Edit node found under Notepad")
    path = node.get("id")
    if not isinstance(path, str) or not path:
        fail("Notepad text node has no path id")
    runtime_id = node.get("runtimeId") or ""
    print(f"{path}\t{runtime_id}")


if __name__ == "__main__":
    main()
