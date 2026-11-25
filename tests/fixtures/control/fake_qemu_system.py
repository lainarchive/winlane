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


def split_qemu_options(value):
    parts = []
    current = []
    index = 0
    while index < len(value):
        if value[index] != ",":
            current.append(value[index])
            index += 1
            continue
        if index + 1 < len(value) and value[index + 1] == ",":
            current.append(",")
            index += 2
        else:
            parts.append("".join(current))
            current = []
            index += 1
    parts.append("".join(current))
    return parts


def qga_socket_path(args):
    for index, arg in enumerate(args[:-1]):
        if arg != "-chardev":
            continue
        parts = split_qemu_options(args[index + 1])
        if len(parts) < 2 or parts[0] != "socket" or parts[1] != "id=qga0":
            continue
        for part in parts[2:]:
            if part.startswith("path="):
                return part[5:]
    return None


def main():
    args = sys.argv[1:]
    qmp_arg = argument_after(args, "-qmp")
    if not qmp_arg or not qmp_arg.startswith("unix:"):
        print("missing unix QMP socket argument", file=sys.stderr)
        return 2
    socket_path = split_qemu_options(qmp_arg[5:])[0]
    pidfile = argument_after(args, "-pidfile")
    if pidfile:
        with open(pidfile, "w", encoding="utf-8") as output:
            output.write(str(os.getpid()))
    with open(socket_path + ".argv", "w", encoding="utf-8") as output:
        output.write(json.dumps(args) + "\n")

    try:
        os.unlink(socket_path)
    except FileNotFoundError:
        pass

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(socket_path)
    server.listen(4)
    qga_server = None
    qga_path = qga_socket_path(args)
    if qga_path:
        try:
            os.unlink(qga_path)
        except FileNotFoundError:
            pass
        qga_server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        qga_server.bind(qga_path)
        qga_server.listen(4)
    log_path = socket_path + ".commands"
    snapshot_job_ids = []
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
                elif execute in {"snapshot-save", "snapshot-load"}:
                    snapshot_job_ids.append(arguments.get("job-id", "checkpoint-fake"))
                    reply(connection, identifier, {"return": {}})
                elif execute == "cont":
                    reply(connection, identifier, {"return": {}})
                elif execute == "query-jobs":
                    reply(connection, identifier, {"return": [{"id": job_id, "type": "snapshot", "status": "concluded", "current-progress": 1, "total-progress": 1} for job_id in snapshot_job_ids]})
                elif execute == "job-dismiss":
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

    def accept_qga():
        while not stopping.is_set():
            try:
                client, _ = qga_server.accept()
                client.close()
            except OSError:
                return

    if qga_server:
        threading.Thread(target=accept_qga, daemon=True).start()

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
