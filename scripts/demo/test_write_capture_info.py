#!/usr/bin/env python3
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import write_capture_info


def probe_json(width=1920, height=1080, duration=10.0, frame_rate="30/1"):
    return json.dumps({
        "streams": [{
            "codec_type": "video",
            "codec_name": "h264",
            "width": width,
            "height": height,
            "avg_frame_rate": frame_rate,
        }],
        "format": {"duration": str(duration)},
    })


class CaptureInfoTest(unittest.TestCase):
    def test_writes_hash_sidecar_commands_environment_and_realtime_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mp4 = root / "winlane-demo-x11.mp4"
            mp4.write_bytes(b"capture bytes")
            output = root / "capture-info.json"
            env = {
                "WINLANE_CAPTURE_STARTED_UNIX": "100",
                "WINLANE_CAPTURE_FINISHED_UNIX": "110",
                "WINLANE_CAPTURE_RECORDER_COMMAND": "record command",
                "WINLANE_CAPTURE_FFMPEG_COMMAND": "ffmpeg command",
                "WINLANE_CAPTURE_OS": "Linux test",
                "WINLANE_CAPTURE_FFMPEG_VERSION": "ffmpeg version test",
                "WINLANE_CAPTURE_WINLANE_BIN": "/bin/winlane",
                "WINLANE_CAPTURE_WINLANE_SHA256": "0" * 64,
                "WINLANE_CAPTURE_WINLANE_VERSION": "winlane test",
                "WINLANE_CAPTURE_RECORD_SCRIPT": "/repo/record-x11.sh",
                "WINLANE_CAPTURE_RECORD_SCRIPT_SHA256": "1" * 64,
                "WINLANE_CAPTURE_DRIVER_SCRIPT": "/repo/drive-demo.sh",
                "WINLANE_CAPTURE_DRIVER_SCRIPT_SHA256": "2" * 64,
            }
            completed = mock.Mock(stdout=probe_json())
            argv = ["write_capture_info.py", str(mp4), str(output), "/kitty", "0.51.1", ":170", "0"]
            with mock.patch.dict(os.environ, env, clear=True), \
                 mock.patch.object(write_capture_info.sys, "argv", argv), \
                 mock.patch.object(write_capture_info.subprocess, "run", return_value=completed):
                write_capture_info.main()
            info = json.loads(output.read_text())
            digest = "e8873af2f1f29756578867d0f90bd3dc73e653f39f7cda5603d3c012d11b4ecb"
            self.assertEqual(info["capture"]["sha256"], digest)
            self.assertEqual((root / "winlane-demo-x11.mp4.sha256").read_text(), f"{digest}  winlane-demo-x11.mp4\n")
            self.assertEqual(info["commands"], {"recorder": "record command", "ffmpeg": "ffmpeg command"})
            self.assertEqual(info["environment"]["winlane_sha256"], "0" * 64)
            self.assertEqual(info["capture"]["duration_wall_clock_ratio"], 1.0)
            self.assertFalse(info["capture"]["time_compression"])
            self.assertEqual(info["capture"]["source_scaling"], "1:1 x11grab pixels from the validated 1920x1080 Xvfb display")

    def run_failure(self, **kwargs):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mp4 = root / "winlane-demo-x11.mp4"
            mp4.write_bytes(b"capture bytes")
            output = root / "capture-info.json"
            env = {
                "WINLANE_CAPTURE_STARTED_UNIX": str(kwargs.pop("started", 100.0)),
                "WINLANE_CAPTURE_FINISHED_UNIX": str(kwargs.pop("finished", 110.0)),
            }
            completed = mock.Mock(stdout=probe_json(**kwargs))
            argv = ["write_capture_info.py", str(mp4), str(output), "/kitty", "0.51.1", ":170", "0"]
            with mock.patch.dict(os.environ, env, clear=True), \
                 mock.patch.object(write_capture_info.sys, "argv", argv), \
                 mock.patch.object(write_capture_info.subprocess, "run", return_value=completed):
                with self.assertRaises(SystemExit):
                    write_capture_info.main()

    def test_rejects_time_compressed_capture(self):
        self.run_failure(duration=5.0)

    def test_rejects_scaled_capture(self):
        self.run_failure(width=1280, height=720)

    def test_rejects_changed_frame_rate(self):
        self.run_failure(frame_rate="60/1")

    def test_requires_command_and_environment_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mp4 = root / "winlane-demo-x11.mp4"
            mp4.write_bytes(b"capture bytes")
            output = root / "capture-info.json"
            completed = mock.Mock(stdout=probe_json())
            argv = ["write_capture_info.py", str(mp4), str(output), "/kitty", "0.51.1", ":170", "0"]
            env = {
                "WINLANE_CAPTURE_STARTED_UNIX": "100",
                "WINLANE_CAPTURE_FINISHED_UNIX": "110",
            }
            with mock.patch.dict(os.environ, env, clear=True), \
                 mock.patch.object(write_capture_info.sys, "argv", argv), \
                 mock.patch.object(write_capture_info.subprocess, "run", return_value=completed):
                with self.assertRaisesRegex(SystemExit, "WINLANE_CAPTURE_RECORDER_COMMAND"):
                    write_capture_info.main()


if __name__ == "__main__":
    unittest.main()
