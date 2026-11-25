#!/usr/bin/env python3
import argparse
import importlib.util
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

FPS_VALUES = [5, 15, 30]


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


def tree_cpu_ticks(benchmark, root_pid):
    total = 0
    for pid in process_tree_pids(root_pid):
        value = benchmark.cpu_ticks(pid)
        if value is not None:
            total += value
    return total


def tree_rss_kb(benchmark, root_pid):
    return sum(benchmark.rss_kb(pid) for pid in process_tree_pids(root_pid))


def write_launcher(root, winlane_bin, socket_path, machine_json, fps):
    command = (
        "#!/bin/sh\nexec "
        + shlex.quote(str(winlane_bin))
        + " view --control-socket "
        + shlex.quote(str(socket_path))
        + " --machine-json "
        + shlex.quote(machine_json)
        + f" --fps {fps} --zoom 100\n"
    )
    launcher = root / f"view-{fps}.sh"
    launcher.write_text(command)
    launcher.chmod(0o755)
    return launcher


def launch_real_kitty(cropped, display, kitty_bin, root, launcher, fps):
    session = root / f"kitty-{fps}.session"
    session.write_text(
        "os_window_size 1200 800\n"
        "os_window_position 0x0\n"
        "layout splits\n"
        f"launch --hold --title winlane-view /bin/sh {shlex.quote(str(launcher))}\n"
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
        stdout=(root / f"kitty-{fps}.log").open("wb"),
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    cropped.wait_for("kitty window", lambda: bool(cropped.window_ids(display) - before), timeout=8)
    return process


def measure_case(cropped, benchmark, root, display, kitty_bin, server, winlane_bin, machine_json, fps, duration):
    launcher = write_launcher(root, winlane_bin, server.server_address, machine_json, fps)
    kitty = launch_real_kitty(cropped, display, kitty_bin, root, launcher, fps)
    try:
        baseline_captures = server.capture_count
        cropped.wait_for("initial terminal repaint", lambda: server.capture_count > baseline_captures, timeout=8)
        time.sleep(0.8)
        viewer_pid = cropped.wait_for("viewer process", lambda: benchmark.find_viewer_pid(winlane_bin), timeout=5)
        captures_before = server.capture_count
        capture_ms_before = len(server.capture_ms)
        started = time.perf_counter()
        last_time = started
        viewer_ticks = benchmark.cpu_ticks(viewer_pid)
        kitty_ticks = tree_cpu_ticks(benchmark, kitty.pid)
        viewer_cpu_max = kitty_cpu_max = 0.0
        viewer_rss_max = kitty_rss_max = 0
        while time.perf_counter() - started < duration:
            time.sleep(0.1)
            now = time.perf_counter()
            new_viewer_ticks = benchmark.cpu_ticks(viewer_pid)
            new_kitty_ticks = tree_cpu_ticks(benchmark, kitty.pid)
            elapsed = now - last_time
            if elapsed > 0:
                if new_viewer_ticks is not None and viewer_ticks is not None:
                    viewer_cpu_max = max(viewer_cpu_max, 100.0 * (new_viewer_ticks - viewer_ticks) / os.sysconf("SC_CLK_TCK") / elapsed)
                kitty_cpu_max = max(kitty_cpu_max, 100.0 * (new_kitty_ticks - kitty_ticks) / os.sysconf("SC_CLK_TCK") / elapsed)
            viewer_ticks, kitty_ticks, last_time = new_viewer_ticks, new_kitty_ticks, now
            viewer_rss_max = max(viewer_rss_max, benchmark.rss_kb(viewer_pid))
            kitty_rss_max = max(kitty_rss_max, tree_rss_kb(benchmark, kitty.pid))
        elapsed = time.perf_counter() - started
        captures = server.capture_count - captures_before
        service = server.capture_ms[capture_ms_before:]
        screenshot = root / f"kitty-{fps}.ppm"
        cropped.capture_screen(display, screenshot)
        _, _, raster = cropped.parse_ppm(screenshot)
        sequence = server.capture_count
        recent_colors = [((n * 3) % 256, (n * 5) % 256, (n * 7) % 256) for n in range(max(1, sequence - 8), sequence + 1)]
        exact_pixels = max(raster.count(bytes(color)) for color in recent_colors)
        return {
            "requested_fps": fps,
            "duration_seconds": round(elapsed, 3),
            "captures": captures,
            "achieved_capture_fps": round(captures / elapsed, 3),
            "capture_deficit": round(fps * elapsed - captures, 2),
            "viewer_cpu_percent_max": round(viewer_cpu_max, 1),
            "kitty_cpu_percent_max": round(kitty_cpu_max, 1),
            "viewer_rss_kb_max": viewer_rss_max,
            "kitty_rss_kb_max": kitty_rss_max,
            "fake_capture_service_ms_avg": round(sum(service) / len(service), 3) if service else 0,
            "latest_frame_color_pixels": exact_pixels,
        }
    finally:
        cropped.process_group_terminate(kitty)
        time.sleep(0.2)


def main():
    parser = argparse.ArgumentParser(description="measure real kitty image decode/render throughput under Xvfb")
    parser.add_argument("--winlane-bin", type=Path)
    parser.add_argument("--kitty-bin", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--duration", type=float, default=3.0)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    benchmark = load_script(repo / "scripts" / "benchmark-live-view.py", "live_benchmark")
    cropped = load_script(repo / "scripts" / "validate-cropped-repaint.py", "cropped_validation")
    winlane_bin = (args.winlane_bin or repo / "target" / "release" / "winlane").resolve()
    check(winlane_bin.is_file(), f"winlane binary missing: {winlane_bin}")
    kitty_bin = args.kitty_bin.resolve() if args.kitty_bin else cropped.download_portable_kitty()
    for dependency in ["Xvfb", "ffmpeg", "xdotool", "xwininfo", "xdpyinfo"]:
        check(shutil.which(dependency), f"missing required host tool: {dependency}")
    root = args.output_dir.resolve() if args.output_dir else Path(tempfile.mkdtemp(prefix="winlane-terminal-decode."))
    root.mkdir(parents=True, exist_ok=True)
    display = None
    xvfb = None
    server = None
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
        machine = {"id": "terminal-decode", "name": "terminal-decode", "state_dir": str(root), "backend": "qemu", "state": "running"}
        machine_json = json.dumps(machine, separators=(",", ":"))
        results = {}
        for fps in FPS_VALUES:
            results[str(fps)] = measure_case(cropped, benchmark, root, display, kitty_bin, server, winlane_bin, machine_json, fps, args.duration)
            check(results[str(fps)]["latest_frame_color_pixels"] > 100, f"visible correctness failed at {fps} fps")
        output = {
            "terminal": "kitty 0.51.1 under Xvfb, real image decode/render path",
            "frame": {"width": benchmark.WIDTH, "height": benchmark.HEIGHT, "format": "P6 PPM", "content": "deterministic synthetic whole-changing"},
            "visible_correctness": "exact latest-or-recent mutation color was present in an X11 screenshot (>100 pixels required)",
            "results": results,
        }
        (root / "result.json").write_text(json.dumps(output, indent=2) + "\n")
        print(json.dumps(output, indent=2))
        print(f"output_dir: {root}")
        return 0
    finally:
        if server is not None:
            server.shutdown()
            server.server_close()
        if xvfb is not None:
            cropped.process_group_terminate(xvfb)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"benchmark-terminal-decode.py: error: {error}", file=sys.stderr)
        raise SystemExit(2)
