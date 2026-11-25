#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
import shlex
import signal
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

WIDTH = 1024
HEIGHT = 768
CROP_X = WIDTH // 4
CROP_Y = HEIGHT // 4
CROP_WIDTH = WIDTH // 2
CROP_HEIGHT = HEIGHT // 2
SCENARIOS = ["whole-changing", "unchanged", "outside-only", "inside-changing"]
FPS_VALUES = [5, 15, 30]


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def base_frame():
    rows = []
    for y in range(HEIGHT):
        row = bytearray()
        for x in range(WIDTH):
            texture = (x // 7 + y // 11) % 23
            row.extend((
                (x // 4 + y // 8 + texture) % 251,
                (x // 5 + y // 3 + 80) % 241,
                (x // 9 + y // 6 + 160) % 229,
            ))
        rows.append(bytes(row))
    return b"".join(rows)


def mutate_frame(base, scenario, sequence):
    if scenario == "unchanged":
        return base
    frame = bytearray(base)
    if scenario == "outside-only":
        x0, y0, width, height = 24 + sequence % 32, 24, 128, 96
    elif scenario == "inside-changing":
        x0 = CROP_X + 32 + sequence % 32
        y0 = CROP_Y + 32
        width, height = 128, 96
    else:
        x0 = 96 + sequence % 48
        y0 = 96
        width, height = 256, 160
    width = min(width, WIDTH - x0)
    pixel = bytes(((sequence * 3) % 256, (sequence * 5) % 256, (sequence * 7) % 256))
    patch = pixel * width
    for y in range(y0, min(y0 + height, HEIGHT)):
        offset = (y * WIDTH + x0) * 3
        frame[offset : offset + len(patch)] = patch
    marker = sequence % WIDTH
    frame[marker * 3] = sequence % 256
    return bytes(frame)


def ppm(payload):
    return f"P6\n{WIDTH} {HEIGHT}\n255\n".encode() + payload


class ControlServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def __init__(self, socket_path, root):
        self.root = root
        self.base = base_frame()
        self.scenario = "whole-changing"
        self.capture_count = 0
        self.capture_ms = []
        self.lock = threading.Lock()
        super().__init__(str(socket_path), ControlHandler)


class ControlHandler(socketserver.StreamRequestHandler):
    def handle(self):
        line = self.rfile.readline()
        try:
            request = json.loads(line)
            if request.get("op") == "capture_frame":
                result = self.capture_frame()
            else:
                result = {}
            response = {"ok": True, "result": result, "error": None}
        except Exception as error:
            response = {"ok": False, "error": repr(error)}
        self.wfile.write((json.dumps(response) + "\n").encode())

    def capture_frame(self):
        started = time.perf_counter()
        server = self.server
        with server.lock:
            server.capture_count += 1
            sequence = server.capture_count
            scenario = server.scenario
        blob = ppm(mutate_frame(server.base, scenario, sequence))
        path = server.root / f"frame-{sequence}.ppm"
        path.write_bytes(blob)
        digest = hashlib.sha256(blob).hexdigest()
        elapsed_ms = (time.perf_counter() - started) * 1000
        with server.lock:
            server.capture_ms.append(elapsed_ms)
        return {
            "sha256": digest,
            "width": WIDTH,
            "height": HEIGHT,
            "captured_unix_ms": int(time.time() * 1000),
            "blob_path": str(path),
        }


def find_viewer_pid(winlane_bin):
    result = subprocess.run(["ps", "-eo", "pid=,args="], text=True, capture_output=True, check=True)
    prefix = str(winlane_bin) + " view "
    for line in result.stdout.splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and parts[1].startswith(prefix):
            return int(parts[0])
    return None


def cpu_ticks(pid):
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
        fields = text[text.rindex(")") + 2 :].split()
        return int(fields[11]) + int(fields[12])
    except (FileNotFoundError, ValueError, IndexError):
        return None


def rss_kb(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except FileNotFoundError:
        pass
    return 0


def terminate(process):
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=2)


def run_case(root, server, winlane_bin, machine_json, scenario, fps, duration):
    server.scenario = scenario
    raw_output = root / f"{scenario}-{fps}.out"
    stderr_path = root / f"{scenario}-{fps}.stderr"
    machine_arg = shlex.quote(machine_json)
    inner = (
        "stty cols 80 rows 24; "
        "exec env TERM=xterm-kitty KITTY_WINDOW_ID=1 COLORTERM=truecolor "
        f"{shlex.quote(str(winlane_bin))} view --control-socket {shlex.quote(server.server_address)} "
        f"--machine-json {machine_arg} --fps {fps} --zoom {'200' if scenario in ('outside-only', 'inside-changing') else '100'} "
        "--cell-width-px 8 --cell-height-px 16"
    )
    with raw_output.open("wb") as stdout, stderr_path.open("wb") as stderr:
        process = subprocess.Popen(
            ["script", "-qefc", inner, "/dev/null"],
            stdout=stdout,
            stderr=stderr,
            start_new_session=True,
        )
        try:
            time.sleep(0.8)
            pid = None
            for _ in range(20):
                pid = find_viewer_pid(winlane_bin)
                if pid is not None:
                    break
                time.sleep(0.05)
            check(pid is not None, f"viewer did not start for {scenario} at {fps} fps")
            time.sleep(0.3)
            captures_before = server.capture_count
            bytes_before = raw_output.stat().st_size
            capture_ms_before = len(server.capture_ms)
            ticks_before = cpu_ticks(pid)
            started = time.perf_counter()
            max_cpu = 0.0
            max_rss = 0
            last_ticks = ticks_before
            last_time = started
            while time.perf_counter() - started < duration:
                time.sleep(0.1)
                now = time.perf_counter()
                ticks = cpu_ticks(pid)
                if ticks is not None and last_ticks is not None and now > last_time:
                    cpu = 100.0 * (ticks - last_ticks) / os.sysconf("SC_CLK_TCK") / (now - last_time)
                    max_cpu = max(max_cpu, cpu)
                    last_ticks = ticks
                    last_time = now
                max_rss = max(max_rss, rss_kb(pid))
            elapsed = time.perf_counter() - started
            captures = server.capture_count - captures_before
            output_bytes = raw_output.stat().st_size - bytes_before
            service_samples = server.capture_ms[capture_ms_before:]
        finally:
            terminate(process)
    service_avg = sum(service_samples) / len(service_samples) if service_samples else 0.0
    return {
        "requested_fps": fps,
        "duration_seconds": round(elapsed, 3),
        "captures": captures,
        "achieved_capture_fps": round(captures / elapsed, 3),
        "output_bytes": output_bytes,
        "output_mib_per_second": round(output_bytes / elapsed / 1024 / 1024, 3),
        "output_bytes_per_capture": round(output_bytes / captures, 1) if captures else 0,
        "viewer_cpu_percent_max": round(max_cpu, 1),
        "viewer_rss_kb_max": max_rss,
        "fake_capture_service_ms_avg": round(service_avg, 3),
        "stderr": stderr_path.read_text(errors="replace")[-400:],
    }


def main():
    parser = argparse.ArgumentParser(description="benchmark the synthetic live-view capture to Kitty-render path")
    parser.add_argument("--winlane-bin", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--duration", type=float, default=3.0)
    parser.add_argument("--scenario", choices=SCENARIOS)
    parser.add_argument("--fps", type=int, choices=FPS_VALUES)
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    winlane_bin = args.winlane_bin or repo / "target" / "release" / "winlane"
    winlane_bin = winlane_bin.resolve()
    check(winlane_bin.is_file() and os.access(winlane_bin, os.X_OK), f"winlane binary missing: {winlane_bin}")
    check(subprocess.run(["script", "--version"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0, "missing required host tool: script")

    root = args.output_dir.resolve() if args.output_dir else Path(tempfile.mkdtemp(prefix="winlane-live-benchmark."))
    root.mkdir(parents=True, exist_ok=True)
    socket_path = root / "control.sock"
    server = ControlServer(socket_path, root)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    machine = {
        "id": "live-benchmark",
        "name": "live-benchmark",
        "state_dir": str(root),
        "backend": "qemu",
        "state": "running",
    }
    machine_json = json.dumps(machine, separators=(",", ":"))
    scenarios = [args.scenario] if args.scenario else SCENARIOS
    fps_values = [args.fps] if args.fps else FPS_VALUES
    results = {}
    try:
        for scenario in scenarios:
            results[scenario] = {}
            for fps in fps_values:
                results[scenario][str(fps)] = run_case(root, server, winlane_bin, machine_json, scenario, fps, args.duration)
        output = {
            "winlane_bin": str(winlane_bin),
            "frame": {"width": WIDTH, "height": HEIGHT, "format": "P6 PPM"},
            "terminal": {"cols": 80, "rows": 24, "cell_width_px": 8, "cell_height_px": 16, "consumer": "fast pseudo-terminal capture, not a real terminal renderer"},
            "zoom": {"whole-changing": 100, "unchanged": 100, "outside-only": 200, "inside-changing": 200},
            "results": results,
        }
        (root / "result.json").write_text(json.dumps(output, indent=2) + "\n")
        print(json.dumps(output, indent=2))
        print(f"output_dir: {root}")
        return 0
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"benchmark-live-view.py: error: {error}", file=sys.stderr)
        raise SystemExit(2)
