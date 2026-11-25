#!/usr/bin/env python3
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

MINIMUM_REALTIME_RATIO = 0.75


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def required_env(name):
    value = os.environ.get(name)
    if not value:
        raise SystemExit(f"missing capture provenance metadata: {name}")
    return value


def main():
    mp4, output, kitty_bin, kitty_version, display, driver_status = sys.argv[1:7]
    probe = subprocess.run(
        [
            "ffprobe", "-v", "error", "-print_format", "json",
            "-show_format", "-show_streams", mp4,
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    data = json.loads(probe.stdout)
    video = next((stream for stream in data.get("streams", []) if stream.get("codec_type") == "video"), None)
    if video is None:
        raise SystemExit("no video stream in capture")
    width = int(video.get("width", 0))
    height = int(video.get("height", 0))
    duration = float(data.get("format", {}).get("duration", 0))
    if (width, height) != (1920, 1080):
        raise SystemExit(f"unexpected capture dimensions: {width}x{height}")
    if duration <= 0.5:
        raise SystemExit(f"capture duration is too short: {duration}")
    if video.get("avg_frame_rate") != "30/1":
        raise SystemExit(f"unexpected output frame rate: {video.get('avg_frame_rate')}")

    started = float(required_env("WINLANE_CAPTURE_STARTED_UNIX"))
    finished = float(required_env("WINLANE_CAPTURE_FINISHED_UNIX"))
    wall_seconds = finished - started
    realtime_ratio = duration / wall_seconds if wall_seconds > 0 else 0
    if realtime_ratio < MINIMUM_REALTIME_RATIO:
        raise SystemExit(
            f"capture appears time-compressed: MP4 duration {duration:.3f}s vs wall clock {wall_seconds:.3f}s"
        )

    digest = sha256_file(mp4)
    mp4_path = Path(mp4)
    with open(f"{mp4}.sha256", "w", encoding="utf-8") as stream:
        stream.write(f"{digest}  {mp4_path.name}\n")

    info = {
        "capture": {
            "file": mp4,
            "sha256": digest,
            "sha256_sidecar": f"{mp4}.sha256",
            "width": width,
            "height": height,
            "duration_seconds": duration,
            "wall_clock_seconds": wall_seconds,
            "duration_wall_clock_ratio": realtime_ratio,
            "codec": video.get("codec_name"),
            "input_framerate": 30,
            "output_avg_frame_rate": video.get("avg_frame_rate"),
            "time_compression": False,
            "time_compression_validation": f"duration/wall-clock ratio {realtime_ratio:.3f} >= {MINIMUM_REALTIME_RATIO}",
            "crop": None,
            "crop_reason": "kitty occupies the complete bare Xvfb root window at capture size",
            "source_scaling": "1:1 x11grab pixels from the validated 1920x1080 Xvfb display",
            "start_after_interactive_readiness": True,
            "start_disclosure": "ffmpeg starts only after the driver proves interactive desktop/UIA readiness; initial boot is not presented as part of the recorded action sequence",
            "started_unix_time": started,
            "finished_unix_time": finished,
        },
        "commands": {
            "recorder": required_env("WINLANE_CAPTURE_RECORDER_COMMAND"),
            "ffmpeg": required_env("WINLANE_CAPTURE_FFMPEG_COMMAND"),
        },
        "environment": {
            "os": required_env("WINLANE_CAPTURE_OS"),
            "ffmpeg_version": required_env("WINLANE_CAPTURE_FFMPEG_VERSION"),
            "winlane_bin": required_env("WINLANE_CAPTURE_WINLANE_BIN"),
            "winlane_sha256": required_env("WINLANE_CAPTURE_WINLANE_SHA256"),
            "winlane_version": required_env("WINLANE_CAPTURE_WINLANE_VERSION"),
            "record_script": required_env("WINLANE_CAPTURE_RECORD_SCRIPT"),
            "record_script_sha256": required_env("WINLANE_CAPTURE_RECORD_SCRIPT_SHA256"),
            "driver_script": required_env("WINLANE_CAPTURE_DRIVER_SCRIPT"),
            "driver_script_sha256": required_env("WINLANE_CAPTURE_DRIVER_SCRIPT_SHA256"),
        },
        "terminal": {
            "kitty_bin": kitty_bin,
            "kitty_version": kitty_version,
            "display": display,
            "multiplexer": None,
            "view_mode": "winlane view directly in kitty",
        },
        "driver": {
            "exit_status": int(driver_status) if driver_status.isdigit() else driver_status,
        },
        "created_unix_time": time.time(),
    }
    with open(output, "w", encoding="utf-8") as stream:
        json.dump(info, stream, indent=2, sort_keys=True)
        stream.write("\n")


if __name__ == "__main__":
    main()
