#!/usr/bin/env python3
import json
import os
import sys


def main():
    args = sys.argv[1:]
    if not args:
        return 2
    if args[0] == "create":
        backing = None
        backing_format = None
        if "-b" in args:
            backing = args[args.index("-b") + 1]
        if "-F" in args:
            backing_format = args[args.index("-F") + 1]
        if backing and backing_format != "qcow2":
            print("backing images must use qcow2 format", file=sys.stderr)
            return 2
        target = args[-2]
        size = args[-1]
        with open(target, "w", encoding="utf-8") as output:
            output.write(json.dumps({"format": "qcow2", "backing": backing, "backing_format": backing_format, "size": size}) + "\n")
        return 0
    if args[0] == "snapshot":
        action = args[1]
        snapshot = args[2]
        target = args[3]
        if not os.path.exists(target):
            print(f"missing disk image: {target}", file=sys.stderr)
            return 1
        with open(target + ".snapshots", "a", encoding="utf-8") as output:
            output.write(json.dumps({"action": action, "snapshot": snapshot}) + "\n")
        return 0
    print("unsupported fake qemu-img operation", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
