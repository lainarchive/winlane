#!/usr/bin/env python3
import json
import os
import signal
import socket
import sys
import threading
import time


def argument_after(args, name):
    try:
        return args[args.index(name) + 1]
    except (ValueError, IndexError):
        return None


def write_ppm(path, width=64, height=48):
    header = f"P6\n{width} {height}\n255\n".encode()
    pixels = bytes((index % 256 for index in range(width * height * 3)))
    with open(path, "wb") as output:
        output.write(header + pixels)


def main():
    args = sys.argv[1:]
    qmp_arg = argument_after(args, "-qmp")
    if not qmp_arg or not qmp_arg.startswith("unix:"):
        print("missing unix QMP socket argument", file=sys.stderr)
        return 2
    socket_path = qmp_arg[5:].split(",", 1)[0]
    pidfile = argument_after(args, "-pidfile")
    if pidfile:
        with open(pidfile, "w", encoding="utf-8") as output:
            output.write(str(os.getpid()))

    try:
        os.unlink(socket_path)
    except FileNotFoundError:
        pass

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(socket_path)
    server.listen(4)
    log_path = socket_path + ".commands"
    stopping = threading.Event()

    def delayed_exit(delay=0.05):
        time.sleep(delay)
        stopping.set()
        try:
            server.close()
        except OSError:
            pass
        os._exit(0)

    def record(command):
        with open(log_path, "a", encoding="utf-8") as log:
            log.write(json.dumps(command, sort_keys=True) + "\n")

    def reply(connection, identifier, payload):
        payload["id"] = identifier
        connection.sendall(json.dumps(payload).encode() + b"\n")

    def handle_client(connection):
        connection.sendall(json.dumps({"QMP": {"version": {"qemu": {"major": 9, "minor": 0, "micro": 0}, "package": "fake"}, "capabilities": []}}).encode() + b"\n")
        buffer = b""
        while not stopping.is_set():
            chunk = connection.recv(65536)
            if not chunk:
                return
            buffer += chunk
            while b"\n" in buffer:
                line, buffer = buffer.split(b"\n", 1)
                if not line:
                    continue
                try:
                    command = json.loads(line)
                except json.JSONDecodeError:
                    continue
                record(command)
                identifier = command.get("id")
                execute = command.get("execute")
                arguments = command.get("arguments") or {}
                if execute == "qmp_capabilities":
                    reply(connection, identifier, {"return": {}})
                    def send_reset():
                        time.sleep(0.02)
                        try:
                            connection.sendall(b'{"event":"RESET","data":{"guest":false},"timestamp":{"seconds":1,"microseconds":2}}\n')
                        except OSError:
                            pass
                    threading.Thread(target=send_reset, daemon=True).start()
                elif execute == "query-status":
                    reply(connection, identifier, {"return": {"status": "running", "running": True, "singlestep": False}})
                elif execute == "screendump":
                    write_ppm(arguments["filename"])
                    reply(connection, identifier, {"return": {}})
                elif execute in {"input-send-event", "send-key", "savevm", "loadvm", "human-monitor-command"}:
                    reply(connection, identifier, {"return": {}})
                    if execute == "input-send-event":
                        connection.sendall(b'{"event":"INPUT_PROCESSED","data":{"device":"virtio-tablet"},"timestamp":{"seconds":1,"microseconds":3}}\n')
                elif execute == "system_powerdown":
                    reply(connection, identifier, {"return": {}})
                    threading.Thread(target=delayed_exit, daemon=True).start()
                elif execute == "quit":
                    reply(connection, identifier, {"return": {}})
                    connection.close()
                    delayed_exit(0)
                else:
                    reply(connection, identifier, {"error": {"class": "CommandNotFound", "desc": f"unsupported fake command {execute}"}})

    signal.signal(signal.SIGTERM, lambda _signum, _frame: delayed_exit(0))
    while not stopping.is_set():
        try:
            client, _ = server.accept()
        except OSError:
            break
        threading.Thread(target=handle_client, args=(client,), daemon=True).start()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
