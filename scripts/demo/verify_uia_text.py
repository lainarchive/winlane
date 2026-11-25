#!/usr/bin/env python3
"""Verify exact text read by the guest-side UIA text task."""

import base64
import json
import sys


def fail(message: str) -> None:
    print(f"verify-uia-text error: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: verify_uia_text.py UIA_RESULT_JSON EXPECTED_TEXT")
    result_path, expected = sys.argv[1], sys.argv[2]
    try:
        with open(result_path, encoding="utf-8-sig") as source:
            result = json.load(source)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read UIA result: {error}")
    if not isinstance(result, dict):
        fail("UIA result is not a JSON object")
    if not result.get("ok"):
        fail(f"guest-side UIA read failed: {result.get('error', 'unknown error')}")
    method = result.get("method")
    if method not in {"ValuePattern", "TextPattern"}:
        fail(f"UIA result has unsupported method: {method!r}")
    encoded = result.get("textBase64")
    if not isinstance(encoded, str):
        fail("UIA result has no textBase64 string")
    try:
        actual = base64.b64decode(encoded, validate=True).decode("utf-8")
    except (ValueError, UnicodeDecodeError) as error:
        fail(f"UIA result text is not valid UTF-8 base64: {error}")
    if actual != expected:
        fail(f"restored text mismatch: expected {expected!r}, got {actual!r}")
    print(f"verified restored Notepad text through {method}: {actual!r}")


if __name__ == "__main__":
    main()
