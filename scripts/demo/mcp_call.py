#!/usr/bin/env python3
"""Send one JSON-RPC tool call to winlane's newline-delimited MCP stdio server."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

PROTOCOL_VERSION = "2024-11-05"


def fail(message: str) -> None:
    print(f"mcp-call error: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="call one winlane MCP tool and print its structured result"
    )
    parser.add_argument("--winlane-bin", required=True, type=Path)
    parser.add_argument("--control-socket", required=True, type=Path)
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--timeout-seconds", type=float, default=30.0)
    parser.add_argument("--arguments", default="{}")
    parser.add_argument("--checkpoint-file", type=Path)
    parser.add_argument("tool")
    args = parser.parse_args()
    if args.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be positive")
    if args.checkpoint_file is not None and args.arguments != "{}":
        parser.error("--arguments and --checkpoint-file are mutually exclusive")
    return args


def load_arguments(args: argparse.Namespace) -> dict:
    if args.checkpoint_file is not None:
        try:
            checkpoint = json.loads(args.checkpoint_file.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            fail(f"cannot read checkpoint file {args.checkpoint_file}: {error}")
        if not isinstance(checkpoint, dict):
            fail("checkpoint file must contain one JSON object")
        return {"checkpoint": checkpoint}
    try:
        arguments = json.loads(args.arguments)
    except json.JSONDecodeError as error:
        fail(f"tool arguments are not valid JSON: {error}")
    if not isinstance(arguments, dict):
        fail("tool arguments must be a JSON object")
    return arguments


def response_error(response: dict):
    if "error" in response:
        error = response["error"]
        if isinstance(error, dict):
            return str(error.get("message", error))
        return str(error)
    result = response.get("result")
    if isinstance(result, dict) and result.get("isError"):
        content = result.get("content")
        if isinstance(content, list) and content:
            text = content[0].get("text") if isinstance(content[0], dict) else None
            if text:
                return str(text)
        return "tool returned an error"
    return None


def main() -> None:
    args = parse_args()
    if not args.winlane_bin.is_file():
        fail(f"winlane binary does not exist: {args.winlane_bin}")
    arguments = load_arguments(args)

    command = [
        str(args.winlane_bin),
        "mcp",
        "--control-socket",
        str(args.control_socket),
    ]
    if args.evidence_dir is not None:
        command.extend(["--evidence-dir", str(args.evidence_dir)])

    messages = [
        {
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "winlane-demo", "version": "0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": args.tool, "arguments": arguments},
        },
    ]
    stdin = "".join(json.dumps(message, separators=(",", ":")) + "\n" for message in messages)

    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        stdout, stderr = process.communicate(stdin, timeout=args.timeout_seconds)
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate()
        fail(f"MCP call timed out after {args.timeout_seconds:g} seconds")
    if process.returncode != 0:
        detail = stderr.strip() or stdout.strip() or f"exit status {process.returncode}"
        fail(f"winlane MCP server failed: {detail}")

    tool_response = None
    for line in stdout.splitlines():
        if not line.strip():
            continue
        try:
            response = json.loads(line)
        except json.JSONDecodeError:
            continue
        if response.get("id") == 1:
            tool_response = response
    if tool_response is None:
        detail = stderr.strip() or "no tool response in MCP stdout"
        fail(detail)

    error = response_error(tool_response)
    if error is not None:
        fail(error)
    result = tool_response.get("result", {})
    structured = result.get("structuredContent", {}) if isinstance(result, dict) else {}
    print(json.dumps(structured.get("result"), separators=(",", ":")))


if __name__ == "__main__":
    main()
