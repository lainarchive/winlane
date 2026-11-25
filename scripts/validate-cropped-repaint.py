#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import socketserver
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

KITTY_VERSION = "0.51.1"
KITTY_SHA256 = "d309be978f017dbdcccae175d281092fefe7c8bc64478fa620581604ab0b53c0"
KITTY_URL = (
    "https://github.com/kovidgoyal/kitty/releases/download/"
    f"v{KITTY_VERSION}/kitty-{KITTY_VERSION}-x86_64.txz"
)

FRAME_WIDTH = 640
FRAME_HEIGHT = 400
CROP_X = 160
CROP_Y = 100
CROP_WIDTH = 320
CROP_HEIGHT = 200
RED = (220, 0, 0)
YELLOW = (240, 220, 0)


def fail(message):
    raise RuntimeError(message)


def check(condition, message):
    if not condition:
        fail(message)


def frame_bytes(phase, sequence):
    if phase == "a":
        outside = (32, 32, 160)
        crop = RED
    elif phase == "b":
        shade = sequence % 250 + 1
        outside = (shade % 256, (shade * 3) % 256, 32)
        crop = RED
    else:
        outside = (0, 120, 80)
        crop = YELLOW

    outside_pixel = bytes(outside)
    crop_pixel = bytes(crop)
    outside_row = outside_pixel * FRAME_WIDTH
    crop_row = (
        outside_pixel * CROP_X
        + crop_pixel * CROP_WIDTH
        + outside_pixel * (FRAME_WIDTH - CROP_X - CROP_WIDTH)
    )
    rows = []
    for y in range(FRAME_HEIGHT):
        if CROP_Y <= y < CROP_Y + CROP_HEIGHT:
            rows.append(crop_row)
        else:
            rows.append(outside_row)
    header = f"P6\n{FRAME_WIDTH} {FRAME_HEIGHT}\n255\n".encode()
    return header + b"".join(rows)


class ControlServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def __init__(self, socket_path, root):
        self.root = root
        self.phase = "a"
        self.capture_count = 0
        self.capture_hashes = []
        self.events = []
        self.lock = threading.Lock()
        super().__init__(str(socket_path), ControlHandler)


class ControlHandler(socketserver.StreamRequestHandler):
    def handle(self):
        line = self.rfile.readline()
        try:
            request = json.loads(line)
            operation = request.get("op")
            if operation == "capture_frame":
                result = self.capture_frame()
            elif operation == "control":
                with self.server.lock:
                    self.server.events.append(request.get("event"))
                result = {}
            else:
                result = {}
            response = {"ok": True, "result": result, "error": None}
        except Exception as error:
            response = {"ok": False, "error": repr(error)}
        self.wfile.write((json.dumps(response) + "\n").encode())

    def capture_frame(self):
        server = self.server
        with server.lock:
            server.capture_count += 1
            sequence = server.capture_count
            phase = server.phase
        blob = frame_bytes(phase, sequence)
        path = server.root / f"frame-{sequence}.ppm"
        path.write_bytes(blob)
        digest = hashlib.sha256(blob).hexdigest()
        with server.lock:
            server.capture_hashes.append((phase, digest))
        return {
            "sha256": digest,
            "width": FRAME_WIDTH,
            "height": FRAME_HEIGHT,
            "captured_unix_ms": int(time.time() * 1000),
            "blob_path": str(path),
        }


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs)


def x11(display, args, **kwargs):
    env = os.environ.copy()
    env["DISPLAY"] = display
    return run(args, env=env, **kwargs)


def download_portable_kitty():
    cache_parent = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "winlane-demo"
    target = cache_parent / f"kitty-{KITTY_VERSION}"
    binary = target / "bin" / "kitty"
    archive = target / "kitty.txz"
    if binary.exists() and archive.exists():
        if hashlib.sha256(archive.read_bytes()).hexdigest() == KITTY_SHA256:
            return binary
        shutil.rmtree(target)

    cache_parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="winlane-kitty-download.") as temporary:
        temporary = Path(temporary)
        downloaded = temporary / "kitty.txz"
        with urllib.request.urlopen(KITTY_URL, timeout=60) as response:
            downloaded.write_bytes(response.read())
        check(
            hashlib.sha256(downloaded.read_bytes()).hexdigest() == KITTY_SHA256,
            "downloaded kitty archive did not match its pinned SHA-256",
        )
        extract = temporary / "extract"
        extract.mkdir()
        with tarfile.open(downloaded, "r:xz") as tar:
            tar.extractall(extract)
        replacement = cache_parent / f"kitty-{KITTY_VERSION}.new"
        shutil.rmtree(replacement, ignore_errors=True)
        shutil.move(str(extract), replacement)
        shutil.copy2(downloaded, replacement / "kitty.txz")
        shutil.rmtree(target, ignore_errors=True)
        os.replace(replacement, target)
    check(binary.exists(), f"kitty binary missing after download: {binary}")
    return binary


def parse_ppm(path):
    data = Path(path).read_bytes()
    tokens = []
    position = 0
    while len(tokens) < 4:
        while data[position] in b" \t\r\n":
            position += 1
        start = position
        while data[position] not in b" \t\r\n":
            position += 1
        tokens.append(data[start:position])
    while data[position] in b" \t\r\n":
        position += 1
    check(tokens[0] == b"P6", f"{path} is not a binary PPM")
    width = int(tokens[1])
    height = int(tokens[2])
    check(int(tokens[3]) == 255, f"{path} is not an 8-bit PPM")
    raster = data[position:]
    check(len(raster) == width * height * 3, f"{path} raster length mismatch")
    return width, height, raster


def color_bbox(path, color):
    width, height, raster = parse_ppm(path)
    pixel = bytes(color)
    count = 0
    min_x = width
    min_y = height
    max_x = -1
    max_y = -1
    for y in range(height):
        row = raster[y * width * 3 : (y + 1) * width * 3]
        for x in range(width):
            offset = x * 3
            if row[offset : offset + 3] == pixel:
                count += 1
                min_x = min(min_x, x)
                min_y = min(min_y, y)
                max_x = max(max_x, x)
                max_y = max(max_y, y)
    return {
        "count": count,
        "bbox": None if count == 0 else [min_x, min_y, max_x, max_y],
    }


def bbox_matches_plan(bbox, plan):
    if bbox is None:
        return False
    width = bbox[2] - bbox[0] + 1
    height = bbox[3] - bbox[1] + 1
    return (
        abs(bbox[0] - plan["offset_x"]) <= 2
        and abs(bbox[1] - plan["offset_y"]) <= 2
        and width == plan["frame_width"]
        and height == plan["frame_height"]
    )


def right_edge_color_count(path, color, x_start):
    width, height, raster = parse_ppm(path)
    pixel = bytes(color)
    count = 0
    for y in range(height):
        row = raster[y * width * 3 : (y + 1) * width * 3]
        for x in range(max(0, x_start), width):
            offset = x * 3
            if row[offset : offset + 3] == pixel:
                count += 1
    return count


def image_output(trace_path):
    total = 0
    writes = 0
    if not Path(trace_path).exists():
        return total, writes
    expression = re.compile(r"write\(1, .*, (\d+)\) = ")
    for line in Path(trace_path).read_text(errors="replace").splitlines():
        match = expression.match(line)
        if match and int(match.group(1)) >= 1000:
            total += int(match.group(1))
            writes += 1
    return total, writes


def wait_for(description, predicate, timeout=5.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    fail(f"timed out waiting for {description}")


def capture_screen(display, path):
    run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "x11grab",
            "-video_size",
            "1280x840",
            "-i",
            display,
            "-frames:v",
            "1",
            "-c:v",
            "ppm",
            str(path),
        ]
    )


def xdotool(display, *args):
    x11(display, ["xdotool", *[str(arg) for arg in args]])


def window_ids(display):
    env = os.environ.copy()
    env["DISPLAY"] = display
    result = subprocess.run(
        ["xdotool", "search", "--class", "kitty"],
        text=True,
        capture_output=True,
        env=env,
    )
    return {int(line) for line in result.stdout.split() if line.strip()}


def window_size(display, window_id):
    result = x11(display, ["xwininfo", "-id", str(window_id)])
    width = int(re.search(r"Width: (\d+)", result.stdout).group(1))
    height = int(re.search(r"Height: (\d+)", result.stdout).group(1))
    return width, height


def process_group_terminate(process):
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


def find_viewer_pid(winlane_bin):
    result = subprocess.run(
        ["ps", "-eo", "pid=,ppid=,args="], text=True, capture_output=True, check=True
    )
    binary = str(winlane_bin)
    for line in result.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3 and parts[2].startswith(binary + " view "):
            return int(parts[0])
    return None


def rss_kb(pid):
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1])
    fail(f"process has no VmRSS entry: {pid}")


def render_plan(frame_width, frame_height, cols, rows, cell_width, cell_height):
    surface_width = cols * cell_width
    surface_height = rows * cell_height
    width_constrained = surface_width * frame_height <= surface_height * frame_width
    if width_constrained:
        scaled_width = surface_width
        scaled_height = max(1, frame_height * scaled_width // frame_width)
        scaled_height = min(scaled_height, surface_height)
    else:
        scaled_height = surface_height
        scaled_width = max(1, frame_width * scaled_height // frame_height)
        scaled_width = min(scaled_width, surface_width)
    return {
        "surface_width": surface_width,
        "surface_height": surface_height,
        "frame_width": scaled_width,
        "frame_height": scaled_height,
        "offset_x": (surface_width - scaled_width) // 2,
        "offset_y": (surface_height - scaled_height) // 2,
    }


def parse_geometry(path):
    values = {}
    for line in Path(path).read_text().splitlines():
        key, value = line.split("=", 1)
        values[key] = int(value)
    return values["cols"], values["rows"]


def write_launcher(root, winlane_bin, socket_path, trace_path, machine_json):
    geometry_path = root / "geometry.env"
    command = (
        f"printf 'cols=%s\\nrows=%s\\n' \"$(tput cols)\" \"$(tput lines)\" "
        f"> {shlex_quote(geometry_path)}; "
        "exec strace "
        f"-o {shlex_quote(trace_path)} -e trace=write "
        f"{shlex_quote(winlane_bin)} view "
        f"--control-socket {shlex_quote(socket_path)} "
        f"--machine-json {shlex_quote(machine_json)} "
        "--fps 30 --zoom 200"
    )
    launcher = root / "view-launcher.sh"
    launcher.write_text("#!/bin/sh\n" + command + "\n")
    launcher.chmod(0o755)
    return launcher


def shlex_quote(value):
    import shlex

    return shlex.quote(str(value))


def launch_kitty(display, kitty_bin, root, launcher, split):
    session = root / ("kitty-split.session" if split else "kitty.session")
    lines = [
        "os_window_size 1200 800",
        "os_window_position 0x0",
        "os_window_title winlane cropped repaint validation",
        "layout splits",
        f"launch --hold --title winlane-view /bin/sh {shlex_quote(launcher)}",
    ]
    if split:
        shell = root / "split-shell.sh"
        shell.write_text("#!/bin/sh\nprintf 'split probe\\n'\nsleep 300\n")
        shell.chmod(0o755)
        lines.append(
            f"launch --location=vsplit --bias=38 --hold --title probe-shell /bin/sh {shlex_quote(shell)}"
        )
    session.write_text("\n".join(lines) + "\n")
    before = window_ids(display)
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

    def new_window():
        current = window_ids(display) - before
        return min(current) if current else None

    window_id = wait_for("kitty window", new_window, timeout=8)
    return process, window_id


def main():
    parser = argparse.ArgumentParser(
        description="validate cropped live-view repaint suppression in a real kitty terminal"
    )
    parser.add_argument("--winlane-bin", type=Path)
    parser.add_argument("--kitty-bin", type=Path)
    parser.add_argument("--output-dir", type=Path)
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    winlane_bin = args.winlane_bin
    if winlane_bin is None:
        release = repo / "target" / "release" / "winlane"
        debug = repo / "target" / "debug" / "winlane"
        winlane_bin = release if release.exists() else debug
    winlane_bin = winlane_bin.resolve()
    check(winlane_bin.is_file() and os.access(winlane_bin, os.X_OK), f"winlane binary missing: {winlane_bin}")

    kitty_bin = args.kitty_bin.resolve() if args.kitty_bin else download_portable_kitty()
    check(kitty_bin.is_file() and os.access(kitty_bin, os.X_OK), f"kitty binary missing: {kitty_bin}")
    for dependency in ["python3", "Xvfb", "ffmpeg", "xdotool", "xwininfo", "xdpyinfo", "strace"]:
        check(shutil.which(dependency), f"missing required host tool: {dependency}")

    if args.output_dir:
        root = args.output_dir.resolve()
        check(not root.exists() or not any(root.iterdir()), f"output directory is not empty: {root}")
        root.mkdir(parents=True, exist_ok=True)
    else:
        root = Path(tempfile.mkdtemp(prefix="winlane-cropped-repaint."))
    root.mkdir(parents=True, exist_ok=True)

    display = None
    xvfb = None
    kitty_process = None
    server = None
    result = {"output_dir": str(root), "winlane_bin": str(winlane_bin), "kitty_bin": str(kitty_bin)}
    try:
        for number in range(90, 100):
            candidate = f":{number}"
            probe = subprocess.run(
                ["xdpyinfo", "-display", candidate],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if probe.returncode != 0:
                display = candidate
                break
        check(display is not None, "no free X display in :90-:99")

        xvfb = subprocess.Popen(
            ["Xvfb", display, "-screen", "0", "1280x840x24"],
            stdout=(root / "xvfb.log").open("wb"),
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        wait_for(
            "Xvfb",
            lambda: subprocess.run(
                ["xwininfo", "-root"], env={**os.environ, "DISPLAY": display}, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            ).returncode == 0,
        )

        socket_path = root / "control.sock"
        server = ControlServer(socket_path, root)
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()

        machine = {
            "id": "cropped-repaint-probe",
            "name": "cropped-repaint-probe",
            "state_dir": str(root),
            "backend": "qemu",
            "state": "running",
        }
        machine_json = json.dumps(machine, separators=(",", ":"))
        trace = root / "trace-single.log"
        launcher = write_launcher(root, winlane_bin, socket_path, trace, machine_json)
        kitty_process, window_id = launch_kitty(display, kitty_bin, root, launcher, split=False)

        wait_for("initial Kitty repaint", lambda: image_output(trace)[0] > 0, timeout=8)
        time.sleep(0.4)
        capture_screen(display, root / "initial.ppm")
        cols, rows = parse_geometry(root / "geometry.env")
        width, height = window_size(display, window_id)
        cell_width = max(1, width // cols)
        cell_height = max(1, height // rows)
        plan = render_plan(CROP_WIDTH, CROP_HEIGHT, cols, rows, cell_width, cell_height)
        expected_pixels = plan["frame_width"] * plan["frame_height"]
        initial_red = color_bbox(root / "initial.ppm", RED)
        check(
            initial_red["count"] >= expected_pixels * 0.99
            and bbox_matches_plan(initial_red["bbox"], plan),
            f"initial red pixels do not match render plan: {initial_red}, expected {expected_pixels} pixels in {plan}",
        )
        result["initial"] = {
            "terminal": [cols, rows, cell_width, cell_height],
            "render_plan": plan,
            "red_pixels": initial_red,
        }

        col = cols // 2
        row = rows // 2
        click_x = col * cell_width + cell_width // 2
        click_y = row * cell_height + cell_height // 2
        point_x = click_x - plan["offset_x"]
        point_y = click_y - plan["offset_y"]
        source_x = point_x * CROP_WIDTH // plan["frame_width"]
        source_y = point_y * CROP_HEIGHT // plan["frame_height"]
        expected_control = {"x": CROP_X + source_x, "y": CROP_Y + source_y}
        xdotool(display, "mousemove", click_x, click_y, "click", "1")
        wait_for(
            "forwarded click",
            lambda: any(event and event.get("kind") == "click" for event in server.events),
        )
        click = next(event for event in server.events if event and event.get("kind") == "click")
        actual_control = {"x": click["x"], "y": click["y"]}
        check(
            actual_control == expected_control,
            f"click inverse mapping {actual_control} != expected {expected_control}",
        )
        result["input"] = {
            "click_pixel": [click_x, click_y],
            "expected_control": expected_control,
            "actual_control": actual_control,
        }

        viewer_pid = wait_for(
            "viewer process", lambda: find_viewer_pid(winlane_bin), timeout=5
        )
        server.phase = "b"
        time.sleep(0.8)
        captures_before = server.capture_count
        bytes_before, writes_before = image_output(trace)
        rss_before = rss_kb(viewer_pid)
        time.sleep(4.0)
        captures_after = server.capture_count
        bytes_after, writes_after = image_output(trace)
        rss_after = rss_kb(viewer_pid)
        capture_screen(display, root / "outside-only.ppm")
        outside_red = color_bbox(root / "outside-only.ppm", RED)
        b_hashes = [digest for phase, digest in server.capture_hashes if phase == "b"]
        outside_captures = captures_after - captures_before
        check(outside_captures >= 10, f"too few outside-only captures: {outside_captures}")
        check(len(set(b_hashes)) > 1, "outside-only frames did not change their artifact hashes")
        check(
            bytes_after == bytes_before and writes_after == writes_before,
            f"outside-only crop changes emitted terminal output: bytes {bytes_before}->{bytes_after}, writes {writes_before}->{writes_after}",
        )
        check(
            outside_red["count"] >= expected_pixels * 0.99
            and bbox_matches_plan(outside_red["bbox"], plan),
            f"outside-only phase changed visible crop geometry: {outside_red}",
        )
        rss_delta = rss_after - rss_before
        check(
            rss_delta <= 2048,
            f"viewer RSS grew by {rss_delta} KiB across {outside_captures} outside-only captures",
        )
        result["outside_only"] = {
            "captures": outside_captures,
            "unique_hashes": len(set(b_hashes)),
            "repaint_bytes_delta": bytes_after - bytes_before,
            "repaint_write_delta": writes_after - writes_before,
            "red_pixels": outside_red,
            "rss_before_kb": rss_before,
            "rss_after_kb": rss_after,
            "rss_delta_kb": rss_delta,
        }

        bytes_before, writes_before = image_output(trace)
        server.phase = "c"
        wait_for(
            "inside-crop repaint",
            lambda: image_output(trace)[0] > bytes_before,
            timeout=3,
        )
        time.sleep(0.4)
        bytes_after, writes_after = image_output(trace)
        capture_screen(display, root / "inside-change.ppm")
        inside_yellow = color_bbox(root / "inside-change.ppm", YELLOW)
        inside_red = color_bbox(root / "inside-change.ppm", RED)["count"]
        check(
            inside_yellow["count"] >= expected_pixels * 0.99
            and bbox_matches_plan(inside_yellow["bbox"], plan)
            and inside_red == 0,
            f"inside crop repaint left stale pixels: yellow={inside_yellow} red={inside_red}",
        )
        repaint_bytes = bytes_after - bytes_before
        result["inside_change"] = {
            "repaint_bytes_delta": repaint_bytes,
            "repaint_write_delta": writes_after - writes_before,
            "yellow_pixels": inside_yellow,
            "stale_red_pixels": inside_red,
        }
        result["outside_only"]["estimated_avoided_bytes"] = outside_captures * repaint_bytes

        bytes_before, _ = image_output(trace)
        xdotool(display, "windowsize", window_id, 1000, 700)
        wait_for(
            "resize repaint",
            lambda: image_output(trace)[0] > bytes_before,
            timeout=4,
        )
        time.sleep(0.4)
        bytes_after, _ = image_output(trace)
        capture_screen(display, root / "after-resize.ppm")
        resize_yellow = color_bbox(root / "after-resize.ppm", YELLOW)
        resize_red = color_bbox(root / "after-resize.ppm", RED)["count"]
        check(
            resize_yellow["count"] > 100_000 and resize_red == 0,
            f"resize repaint incorrect: yellow={resize_yellow['count']} red={resize_red}",
        )
        result["resize"] = {
            "repaint_bytes_delta": bytes_after - bytes_before,
            "yellow_pixels": resize_yellow,
        }

        bytes_before, _ = image_output(trace)
        xdotool(display, "mousemove", 500, 350, "click", "1")
        xdotool(display, "key", "ctrl+plus")
        wait_for(
            "font-zoom repaint",
            lambda: image_output(trace)[0] > bytes_before,
            timeout=4,
        )
        time.sleep(0.4)
        bytes_after, _ = image_output(trace)
        capture_screen(display, root / "after-font-zoom.ppm")
        zoom_yellow = color_bbox(root / "after-font-zoom.ppm", YELLOW)
        zoom_red = color_bbox(root / "after-font-zoom.ppm", RED)["count"]
        check(
            zoom_yellow["count"] > 100_000 and zoom_red == 0,
            f"font-zoom repaint incorrect: yellow={zoom_yellow['count']} red={zoom_red}",
        )
        result["font_zoom"] = {
            "repaint_bytes_delta": bytes_after - bytes_before,
            "yellow_pixels": zoom_yellow,
        }

        process_group_terminate(kitty_process)
        kitty_process = None
        server.phase = "a"
        split_trace = root / "trace-split.log"
        launcher = write_launcher(root, winlane_bin, socket_path, split_trace, machine_json)
        kitty_process, _ = launch_kitty(display, kitty_bin, root, launcher, split=True)
        wait_for("split-pane repaint", lambda: image_output(split_trace)[0] > 0, timeout=8)
        time.sleep(0.5)
        capture_screen(display, root / "split.ppm")
        split_red = color_bbox(root / "split.ppm", RED)
        check(split_red["count"] > 100_000, "split-pane probe did not render enough red pixels")
        check(
            split_red["bbox"] and split_red["bbox"][2] < 850,
            f"split-pane image was not clipped to the left pane: bbox={split_red['bbox']}",
        )
        right_red = right_edge_color_count(root / "split.ppm", RED, 900)
        check(right_red == 0, f"split-pane image leaked {right_red} red pixels into x>=900")
        result["split_pane"] = {
            "red_pixels": split_red,
            "right_edge_red_pixels": right_red,
        }

        result["ok"] = True
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(f"output_dir: {root}")
        print(
            "outside-only: "
            f"{outside_captures} changing-hash captures, "
            f"{result['outside_only']['repaint_bytes_delta']} repaint bytes, "
            f"estimated avoided {result['outside_only']['estimated_avoided_bytes']} bytes, "
            f"RSS delta {rss_delta} KiB"
        )
        print(f"inside-change repaint bytes: {repaint_bytes}")
        print("cropped repaint validation passed")
        return 0
    finally:
        if kitty_process is not None:
            process_group_terminate(kitty_process)
        if server is not None:
            server.shutdown()
            server.server_close()
        if xvfb is not None:
            process_group_terminate(xvfb)
        if not result.get("ok"):
            (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"validate-cropped-repaint.py: error: {error}", file=sys.stderr)
        raise SystemExit(2)
