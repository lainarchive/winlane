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

sys.dont_write_bytecode = True


def load_script(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def xdotool(display, *args):
    env = os.environ.copy()
    env["DISPLAY"] = display
    subprocess.run(["xdotool", *[str(arg) for arg in args]], check=True, env=env, capture_output=True)


def kitty_rc(kitty_bin, listen_on, *args):
    subprocess.run(
        [str(kitty_bin), "@", "--to", f"unix:{listen_on}", *[str(arg) for arg in args]],
        check=True,
        capture_output=True,
    )


def kitty_ls(kitty_bin, listen_on):
    result = subprocess.run(
        [str(kitty_bin), "@", "--to", f"unix:{listen_on}", "ls"],
        check=True,
        text=True,
        capture_output=True,
    )
    return json.loads(result.stdout)


def terminal_cells(kitty_bin, listen_on):
    data = kitty_ls(kitty_bin, listen_on)
    windows = data[0]["tabs"][0]["windows"]
    check(windows, "kitty remote-control listing has no windows")
    window = windows[0]
    return int(window["columns"]), int(window["lines"])


def event_count(server):
    with server.lock:
        return len(server.events)


def events_since(server, start):
    with server.lock:
        return [event for event in server.events[start:] if event]


def wait_for(description, predicate, timeout=4.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.04)
    raise RuntimeError(f"timed out waiting for {description}")


def write_launcher(root, winlane_bin, socket_path, machine_json):
    geometry_path = root / "geometry.env"
    launcher = root / "view-input-launcher.sh"
    launcher.write_text(
        "#!/bin/sh\n"
        f"printf 'cols=%s\\nrows=%s\\n' \"$(tput cols)\" \"$(tput lines)\" > {shlex.quote(str(geometry_path))}\n"
        "exec "
        + shlex.quote(str(winlane_bin))
        + " view --control-socket "
        + shlex.quote(str(socket_path))
        + " --machine-json "
        + shlex.quote(machine_json)
        + " --fps 30 --zoom 200\n"
    )
    launcher.chmod(0o755)
    return launcher


def launch_kitty(display, kitty_bin, root, launcher, listen_on):
    session = root / "kitty-input.session"
    session.write_text(
        "os_window_size 1200 800\n"
        "os_window_position 0x0\n"
        "layout splits\n"
        f"launch --hold --title winlane-input /bin/sh {shlex.quote(str(launcher))}\n"
    )
    env = os.environ.copy()
    env["DISPLAY"] = display
    before = cropped.window_ids(display)
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
            "-o",
            "allow_remote_control=yes",
            "--listen-on",
            f"unix:{listen_on}",
            "--session",
            str(session),
        ],
        stdout=(root / "kitty.log").open("wb"),
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    window_id = cropped.wait_for(
        "kitty window", lambda: bool(cropped.window_ids(display) - before), timeout=8
    )
    ids = cropped.window_ids(display) - before
    return process, min(ids)


def geometry(root, display, window_id, kitty_bin, listen_on):
    cols, rows = terminal_cells(kitty_bin, listen_on)
    width, height = cropped.window_size(display, window_id)
    cell_width = max(1, width // cols)
    cell_height = max(1, height // rows)
    plan = cropped.render_plan(
        cropped.CROP_WIDTH, cropped.CROP_HEIGHT, cols, rows, cell_width, cell_height
    )
    return cols, rows, cell_width, cell_height, plan


def click_point(display, col, row, cell_width, cell_height):
    xdotool(display, "mousemove", col * cell_width + cell_width // 2, row * cell_height + cell_height // 2, "click", "1")


def expected_click(crop_x, crop_y, col, row, cell_width, cell_height, plan):
    x = col * cell_width + cell_width // 2
    y = row * cell_height + cell_height // 2
    point_x = x - plan["offset_x"]
    point_y = y - plan["offset_y"]
    source_x = point_x * cropped.CROP_WIDTH // plan["frame_width"]
    source_y = point_y * cropped.CROP_HEIGHT // plan["frame_height"]
    return {"x": crop_x + source_x, "y": crop_y + source_y}


def clear_events(server):
    with server.lock:
        server.events.clear()


def main():
    parser = argparse.ArgumentParser(description="validate keyboard, paste, mouse, pan, and detach input in real kitty under Xvfb")
    parser.add_argument("--winlane-bin", type=Path)
    parser.add_argument("--kitty-bin", type=Path)
    parser.add_argument("--output-dir", type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    global cropped
    cropped = load_script(repo / "scripts" / "validate-cropped-repaint.py", "cropped_validation")
    winlane_bin = (args.winlane_bin or repo / "target" / "debug" / "winlane").resolve()
    check(winlane_bin.is_file(), f"winlane binary missing: {winlane_bin}")
    kitty_bin = args.kitty_bin.resolve() if args.kitty_bin else cropped.download_portable_kitty()
    for dependency in ["Xvfb", "xdotool", "xwininfo", "xdpyinfo"]:
        check(shutil.which(dependency), f"missing required host tool: {dependency}")
    root = args.output_dir.resolve() if args.output_dir else Path(tempfile.mkdtemp(prefix="winlane-terminal-input."))
    if root.exists() and any(root.iterdir()):
        raise RuntimeError(f"output directory is not empty: {root}")
    root.mkdir(parents=True, exist_ok=True)
    display = None
    xvfb = kitty = server = None
    result = {"terminal": "kitty 0.51.1 under Xvfb", "winlane_bin": str(winlane_bin)}
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
        server = cropped.ControlServer(socket_path, root)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        machine = {"id": "terminal-input", "name": "terminal-input", "state_dir": str(root), "backend": "qemu", "state": "running"}
        machine_json = json.dumps(machine, separators=(",", ":"))
        launcher = write_launcher(root, winlane_bin, socket_path, machine_json)
        listen_on = root / "kitty-rc.sock"
        kitty, window_id = launch_kitty(display, kitty_bin, root, launcher, listen_on)
        baseline = server.capture_count
        cropped.wait_for("initial capture", lambda: server.capture_count > baseline, timeout=8)
        viewer_pid = cropped.wait_for("viewer process", lambda: cropped.find_viewer_pid(winlane_bin), timeout=5)
        time.sleep(0.5)
        cols, rows, cell_width, cell_height, plan = geometry(root, display, window_id, kitty_bin, listen_on)
        result["initial_geometry"] = {"cols": cols, "rows": rows, "cell_width": cell_width, "cell_height": cell_height, "render_plan": plan}

        clear_events(server)
        xdotool(display, "type", "--delay", "20", "aZ09")
        wait_for("physical keyboard text", lambda: len(events_since(server, 0)) >= 4)
        typed = "".join(event.get("text", "") for event in events_since(server, 0) if event.get("kind") == "type")
        check("aZ09" in typed, f"physical keyboard text mismatch: {typed!r}")
        result["keyboard_text"] = typed

        clear_events(server)
        xdotool(display, "key", "Return")
        xdotool(display, "key", "Tab")
        xdotool(display, "key", "ctrl+c")
        wait_for("special keys", lambda: len(events_since(server, 0)) >= 3)
        keys = [event.get("chord") for event in events_since(server, 0) if event.get("kind") == "key"]
        check("enter" in keys and "tab" in keys and "ctrl+c" in keys, f"special key chords mismatch: {keys}")
        result["special_key_chords"] = keys

        clear_events(server)
        kitty_rc(kitty_bin, listen_on, "send-text", "é")
        time.sleep(0.4)
        unicode_events = events_since(server, 0)
        check(not any(event.get("kind") == "type" and "é" in event.get("text", "") for event in unicode_events), f"unsupported Unicode key text was forwarded: {unicode_events}")
        result["unicode_rejection"] = "no type event forwarded for é"

        paste_path = root / "paste-normalized.txt"
        paste_path.write_bytes(b"one\r\ntwo\rthree\tend")
        clear_events(server)
        kitty_rc(kitty_bin, listen_on, "send-text", "--bracketed-paste", "enable", "--from-file", str(paste_path))
        wait_for("normalized paste", lambda: any(event.get("kind") == "type" and "three" in event.get("text", "") for event in events_since(server, 0)))
        paste_text = "".join(event.get("text", "") for event in events_since(server, 0) if event.get("kind") == "type")
        check(paste_text == "one\ntwo\nthree\tend", f"paste normalization mismatch: {paste_text!r}")
        result["paste_normalized"] = paste_text

        bad_path = root / "paste-escape.txt"
        bad_path.write_bytes(b"safe\x1b[2J")
        clear_events(server)
        kitty_rc(kitty_bin, listen_on, "send-text", "--bracketed-paste", "enable", "--from-file", str(bad_path))
        time.sleep(0.5)
        rejected = events_since(server, 0)
        check(not any(event.get("kind") == "type" for event in rejected), f"escape paste was forwarded: {rejected}")
        result["paste_escape_rejection"] = "no type event forwarded"

        col, row = cols // 2, rows // 2
        clear_events(server)
        click_point(display, col, row, cell_width, cell_height)
        wait_for("initial click", lambda: any(event.get("kind") == "click" for event in events_since(server, 0)))
        click = next(event for event in events_since(server, 0) if event.get("kind") == "click")
        expected = expected_click(cropped.CROP_X, cropped.CROP_Y, col, row, cell_width, cell_height, plan)
        actual = {"x": click["x"], "y": click["y"]}
        check(actual == expected, f"initial click inverse mapping {actual} != {expected}")
        result["initial_click"] = {"expected": expected, "actual": actual}

        clear_events(server)
        xdotool(display, "mousemove", col * cell_width + cell_width // 2, row * cell_height + cell_height // 2)
        xdotool(display, "click", "4")
        xdotool(display, "click", "5")
        wait_for("scroll events", lambda: len([event for event in events_since(server, 0) if event.get("kind") == "scroll"]) >= 2)
        directions = [event.get("direction") for event in events_since(server, 0) if event.get("kind") == "scroll"]
        check("up" in directions and "down" in directions, f"scroll directions mismatch: {directions}")
        result["scroll_directions"] = directions

        clear_events(server)
        xdotool(display, "key", "ctrl+Right")
        time.sleep(0.4)
        pan_events = events_since(server, 0)
        check(not any(event.get("kind") == "key" and event.get("chord") == "ctrl+right" for event in pan_events), f"pan key leaked to control plane: {pan_events}")
        click_point(display, col, row, cell_width, cell_height)
        wait_for("post-pan click", lambda: any(event.get("kind") == "click" for event in events_since(server, 0)))
        click = next(event for event in events_since(server, 0) if event.get("kind") == "click")
        pan_crop_x = cropped.CROP_X + cropped.CROP_WIDTH // 4
        expected = expected_click(pan_crop_x, cropped.CROP_Y, col, row, cell_width, cell_height, plan)
        actual = {"x": click["x"], "y": click["y"]}
        check(actual == expected, f"post-pan click inverse mapping {actual} != {expected}")
        result["post_pan_click"] = {"expected": expected, "actual": actual, "pan_key_forwarded": False}

        clear_events(server)
        xdotool(display, "windowsize", window_id, 1000, 700)
        wait_for("resize geometry", lambda: terminal_cells(kitty_bin, listen_on) != (cols, rows), timeout=4)
        time.sleep(0.5)
        resize_cols, resize_rows, resize_cell_width, resize_cell_height, resize_plan = geometry(root, display, window_id, kitty_bin, listen_on)
        resize_col, resize_row = resize_cols // 2, resize_rows // 2
        click_point(display, resize_col, resize_row, resize_cell_width, resize_cell_height)
        wait_for("post-resize click", lambda: any(event.get("kind") == "click" for event in events_since(server, 0)))
        click = next(event for event in events_since(server, 0) if event.get("kind") == "click")
        expected = expected_click(pan_crop_x, cropped.CROP_Y, resize_col, resize_row, resize_cell_width, resize_cell_height, resize_plan)
        actual = {"x": click["x"], "y": click["y"]}
        check(actual == expected, f"post-resize click inverse mapping {actual} != {expected}")
        result["post_resize_click"] = {"geometry": {"cols": resize_cols, "rows": resize_rows, "cell_width": resize_cell_width, "cell_height": resize_cell_height}, "expected": expected, "actual": actual}

        clear_events(server)
        xdotool(display, "key", "ctrl+plus")
        time.sleep(0.8)
        zoom_cols, zoom_rows, zoom_cell_width, zoom_cell_height, zoom_plan = geometry(root, display, window_id, kitty_bin, listen_on)
        check((zoom_cell_width, zoom_cell_height) != (resize_cell_width, resize_cell_height) or (zoom_cols, zoom_rows) != (resize_cols, resize_rows), "font zoom did not change terminal geometry")
        zoom_col, zoom_row = zoom_cols // 2, zoom_rows // 2
        click_point(display, zoom_col, zoom_row, zoom_cell_width, zoom_cell_height)
        wait_for("post-font-zoom click", lambda: any(event.get("kind") == "click" for event in events_since(server, 0)))
        click = next(event for event in events_since(server, 0) if event.get("kind") == "click")
        expected = expected_click(pan_crop_x, cropped.CROP_Y, zoom_col, zoom_row, zoom_cell_width, zoom_cell_height, zoom_plan)
        actual = {"x": click["x"], "y": click["y"]}
        check(actual == expected, f"post-font-zoom click inverse mapping {actual} != {expected}")
        result["post_font_zoom_click"] = {"geometry": {"cols": zoom_cols, "rows": zoom_rows, "cell_width": zoom_cell_width, "cell_height": zoom_cell_height}, "expected": expected, "actual": actual}

        viewer_alive = lambda: cropped.find_viewer_pid(winlane_bin) == viewer_pid
        xdotool(display, "key", "ctrl+q")
        time.sleep(0.5)
        check(viewer_alive(), "viewer detached after one Ctrl-Q")
        xdotool(display, "key", "ctrl+q")
        wait_for("double Ctrl-Q detach", lambda: cropped.find_viewer_pid(winlane_bin) is None, timeout=4)
        result["detach"] = "one Ctrl-Q did not detach; second consecutive Ctrl-Q detached"

        result["ok"] = True
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        print(f"output_dir: {root}")
        print("terminal input validation passed")
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
        print(f"validate-terminal-input.py: error: {error}", file=sys.stderr)
        raise SystemExit(2)
