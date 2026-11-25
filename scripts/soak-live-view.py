#!/usr/bin/env python3
import argparse
import importlib.util
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.dont_write_bytecode = True


def load_script(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def process_tree_pids(root_pid):
    result = subprocess.run(["ps", "-eo", "pid=,ppid="], text=True, capture_output=True, check=True)
    children = {}
    for line in result.stdout.splitlines():
        parts = line.split()
        if len(parts) == 2:
            children.setdefault(int(parts[1]), []).append(int(parts[0]))
    found = []
    pending = [root_pid]
    while pending:
        pid = pending.pop()
        found.append(pid)
        pending.extend(children.get(pid, []))
    return found


def tree_sum(root_pid, reader):
    total = 0
    for pid in process_tree_pids(root_pid):
        value = reader(pid)
        if value is not None:
            total += value
    return total


def fd_count(pid):
    try:
        return len(list(Path(f"/proc/{pid}/fd").iterdir()))
    except FileNotFoundError:
        return None


def terminal_write_bytes(trace_path):
    total = 0
    writes = 0
    if not trace_path.exists():
        return total, writes
    expression = re.compile(r"write\(1, .*\) = (\d+)")
    for line in trace_path.read_text(errors="replace").splitlines():
        match = expression.match(line)
        if match:
            total += int(match.group(1))
            writes += 1
    return total, writes


def write_launcher(root, winlane_bin, socket_path, machine_json, trace_path):
    command = (
        "#!/bin/sh\nexec strace -o "
        + shlex.quote(str(trace_path))
        + " -e trace=write "
        + shlex.quote(str(winlane_bin))
        + " view --control-socket "
        + shlex.quote(str(socket_path))
        + " --machine-json "
        + shlex.quote(machine_json)
        + " --fps 5 --zoom 100\n"
    )
    launcher = root / "view-soak.sh"
    launcher.write_text(command)
    launcher.chmod(0o755)
    return launcher


def launch_kitty(cropped, display, kitty_bin, root, launcher):
    session = root / "kitty-soak.session"
    session.write_text(
        "os_window_size 1200 800\n"
        "os_window_position 0x0\n"
        "layout splits\n"
        f"launch --hold --title winlane-soak /bin/sh {shlex.quote(str(launcher))}\n"
    )
    before = cropped.window_ids(display)
    env = os.environ.copy()
    env["DISPLAY"] = display
    process = subprocess.Popen(
        [
            str(kitty_bin),
            "--config",
            "NONE",
            "-o",
            "font_size=12",
            "-o",
            "confirm_os_window_close=0",
            "-o",
            "shell_integration=disabled",
            "--session",
            str(session),
        ],
        stdout=(root / "kitty.log").open("wb"),
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    cropped.wait_for("kitty window", lambda: bool(cropped.window_ids(display) - before), timeout=8)
    ids = cropped.window_ids(display) - before
    return process, min(ids)


def sample(benchmark, viewer_pid, kitty_pid, trace_path, server, started):
    now = time.perf_counter()
    bytes_written, write_count = terminal_write_bytes(trace_path)
    return {
        "elapsed_seconds": round(now - started, 3),
        "captures": server.capture_count,
        "viewer_cpu_ticks": benchmark.cpu_ticks(viewer_pid),
        "kitty_cpu_ticks": tree_sum(kitty_pid, benchmark.cpu_ticks),
        "viewer_rss_kb": benchmark.rss_kb(viewer_pid),
        "kitty_rss_kb": tree_sum(kitty_pid, benchmark.rss_kb),
        "viewer_fds": fd_count(viewer_pid),
        "kitty_fds": tree_sum(kitty_pid, fd_count),
        "terminal_write_bytes": bytes_written,
        "terminal_writes": write_count,
    }


def percent_cpu(delta_ticks, elapsed):
    if delta_ticks is None or elapsed <= 0:
        return 0.0
    return round(100.0 * delta_ticks / os.sysconf("SC_CLK_TCK") / elapsed, 2)


def growth(first, last, key):
    initial = first[key]
    final = last[key]
    return {"initial": initial, "final": final, "delta": None if initial is None or final is None else final - initial}


def main():
    parser = argparse.ArgumentParser(description="soak the real-kitty live-view path at the supported 5 fps default")
    parser.add_argument("--winlane-bin", type=Path)
    parser.add_argument("--kitty-bin", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--duration", type=float, default=600.0)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    benchmark = load_script(repo / "scripts" / "benchmark-live-view.py", "live_benchmark")
    cropped = load_script(repo / "scripts" / "validate-cropped-repaint.py", "cropped_validation")
    winlane_bin = (args.winlane_bin or repo / "target" / "release" / "winlane").resolve()
    check(winlane_bin.is_file(), f"winlane binary missing: {winlane_bin}")
    kitty_bin = args.kitty_bin.resolve() if args.kitty_bin else cropped.download_portable_kitty()
    for dependency in ["Xvfb", "ffmpeg", "xdotool", "xwininfo", "xdpyinfo", "strace"]:
        check(shutil.which(dependency), f"missing required host tool: {dependency}")
    root = args.output_dir.resolve() if args.output_dir else Path(tempfile.mkdtemp(prefix="winlane-live-soak."))
    if root.exists() and any(root.iterdir()):
        raise RuntimeError(f"output directory is not empty: {root}")
    root.mkdir(parents=True, exist_ok=True)
    display = None
    xvfb = kitty = server = None
    result = {"terminal": "kitty 0.51.1 under Xvfb", "requested_fps": 5, "winlane_bin": str(winlane_bin)}
    try:
        for number in range(90, 100):
            candidate = f":{number}"
            probe = subprocess.run(["xdpyinfo", "-display", candidate], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if probe.returncode != 0:
                display = candidate
                break
        check(display is not None, "no free X display in :90-:99")
        xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", "1280x840x24"], stdout=(root / "xvfb.log").open("wb"), stderr=subprocess.STDOUT, start_new_session=True)
        cropped.wait_for("Xvfb", lambda: subprocess.run(["xwininfo", "-root"], env={**os.environ, "DISPLAY": display}, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0)
        socket_path = root / "control.sock"
        server = benchmark.ControlServer(socket_path, root)
        server.scenario = "whole-changing"
        threading.Thread(target=server.serve_forever, daemon=True).start()
        machine = {"id": "live-soak", "name": "live-soak", "state_dir": str(root), "backend": "qemu", "state": "running"}
        machine_json = json.dumps(machine, separators=(",", ":"))
        trace_path = root / "viewer-writes.trace"
        launcher = write_launcher(root, winlane_bin, socket_path, machine_json, trace_path)
        kitty, window_id = launch_kitty(cropped, display, kitty_bin, root, launcher)
        baseline_captures = server.capture_count
        cropped.wait_for("initial capture", lambda: server.capture_count > baseline_captures, timeout=8)
        viewer_pid = cropped.wait_for("viewer process", lambda: benchmark.find_viewer_pid(winlane_bin), timeout=5)
        time.sleep(1.0)
        started = time.perf_counter()
        first_captures = server.capture_count
        first = sample(benchmark, viewer_pid, kitty.pid, trace_path, server, started)
        samples = [first]
        maximums = {"viewer_rss_kb": first["viewer_rss_kb"], "kitty_rss_kb": first["kitty_rss_kb"], "viewer_fds": first["viewer_fds"], "kitty_fds": first["kitty_fds"]}
        resize_done = font_zoom_done = False
        while time.perf_counter() - started < args.duration:
            time.sleep(1.0)
            elapsed = time.perf_counter() - started
            if not resize_done and elapsed >= args.duration * 0.4:
                cropped.xdotool(display, "windowsize", window_id, 1000, 700)
                resize_done = True
            if not font_zoom_done and elapsed >= args.duration * 0.7:
                cropped.xdotool(display, "mousemove", 500, 350, "click", "1")
                cropped.xdotool(display, "key", "ctrl+plus")
                font_zoom_done = True
            current = sample(benchmark, viewer_pid, kitty.pid, trace_path, server, started)
            samples.append(current)
            for key in maximums:
                if current[key] is not None:
                    maximums[key] = max(maximums[key] or 0, current[key])
        elapsed = time.perf_counter() - started
        last = samples[-1]
        captures = last["captures"] - first_captures
        screenshot = root / "final.ppm"
        cropped.capture_screen(display, screenshot)
        _, _, raster = cropped.parse_ppm(screenshot)
        sequence = server.capture_count
        recent_colors = [((n * 3) % 256, (n * 5) % 256, (n * 7) % 256) for n in range(max(1, sequence - 8), sequence + 1)]
        exact_pixels = max(raster.count(bytes(color)) for color in recent_colors)
        check(exact_pixels > 100, f"visible correctness failed at end: {exact_pixels} exact pixels")

        cropped.xdotool(display, "key", "ctrl+q")
        time.sleep(0.4)
        check(benchmark.find_viewer_pid(winlane_bin) is not None, "viewer detached after one Ctrl-Q")
        cropped.xdotool(display, "key", "ctrl+q")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and benchmark.find_viewer_pid(winlane_bin) is not None:
            time.sleep(0.1)
        check(benchmark.find_viewer_pid(winlane_bin) is None, "viewer did not detach after double Ctrl-Q")

        viewer_ticks = (last["viewer_cpu_ticks"] or 0) - (first["viewer_cpu_ticks"] or 0)
        kitty_ticks = (last["kitty_cpu_ticks"] or 0) - (first["kitty_cpu_ticks"] or 0)
        result.update(
            {
                "duration_seconds": round(elapsed, 3),
                "captures": captures,
                "achieved_capture_fps": round(captures / elapsed, 3),
                "capture_deficit": round(5 * elapsed - captures, 2),
                "viewer_cpu_percent_average": percent_cpu(viewer_ticks, elapsed),
                "kitty_cpu_percent_average": percent_cpu(kitty_ticks, elapsed),
                "viewer_rss_kb": growth(first, last, "viewer_rss_kb") | {"max": maximums["viewer_rss_kb"]},
                "kitty_rss_kb": growth(first, last, "kitty_rss_kb") | {"max": maximums["kitty_rss_kb"]},
                "viewer_fds": growth(first, last, "viewer_fds") | {"max": maximums["viewer_fds"]},
                "kitty_fds": growth(first, last, "kitty_fds") | {"max": maximums["kitty_fds"]},
                "terminal_write_bytes": last["terminal_write_bytes"] - first["terminal_write_bytes"],
                "terminal_writes": last["terminal_writes"] - first["terminal_writes"],
                "terminal_write_mib": round((last["terminal_write_bytes"] - first["terminal_write_bytes"]) / 1024 / 1024, 3),
                "disturbances": ["window resize to 1000x700 at 40% duration", "kitty font zoom at 70% duration"],
                "visible_correctness_exact_recent_frame_pixels": exact_pixels,
                "detach": "one Ctrl-Q did not detach; second consecutive Ctrl-Q detached cleanly",
                "samples": samples[:: max(1, len(samples) // 20)],
                "fake_capture_service_ms_avg": round(sum(server.capture_ms) / len(server.capture_ms), 3) if server.capture_ms else 0,
            }
        )
        check(captures > 0 and result["achieved_capture_fps"] >= 4.75, f"capture continuity failed: {captures} captures, achieved {result['achieved_capture_fps']} fps")
        result["ok"] = True
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({key: value for key, value in result.items() if key != "samples"}, indent=2))
        print(f"output_dir: {root}")
        print("live-view soak validation passed")
        return 0
    finally:
        if kitty is not None:
            cropped.process_group_terminate(kitty)
        if server is not None:
            server.shutdown()
            server.server_close()
        if xvfb is not None:
            cropped.process_group_terminate(xvfb)
        if not result.get("ok"):
            (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"soak-live-view.py: error: {error}", file=sys.stderr)
        raise SystemExit(2)
