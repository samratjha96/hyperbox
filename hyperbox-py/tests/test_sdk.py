from __future__ import annotations

import os
import pathlib
import shutil
import socket
import subprocess
import tempfile
import time
import unittest

from hyperbox.sdk import HyperboxClient


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
HYPERBOX_BIN = REPO_ROOT / "target" / "debug" / "hyperbox"


def free_addr() -> str:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        host, port = sock.getsockname()
        return f"{host}:{port}"


class HyperboxSdkTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        subprocess.run(
            ["cargo", "build", "-p", "hyperbox-cli"],
            cwd=REPO_ROOT,
            check=True,
        )
        cls.addr = free_addr()
        cls.server_url = f"http://{cls.addr}"
        cls.home_dir = pathlib.Path(tempfile.mkdtemp(prefix="hyperbox-py-test."))
        cls.state_db = cls.home_dir / "state.db"
        cls.snapshot_root = cls.home_dir / "snapshots"
        env = os.environ.copy()
        env["HOME"] = str(cls.home_dir)
        env["HYPERBOX_BACKEND"] = "local"
        env["HYPERBOX_STATE_DB"] = str(cls.state_db)
        env["HYPERBOX_SNAPSHOT_ROOT"] = str(cls.snapshot_root)
        cls.server = subprocess.Popen(
            [str(HYPERBOX_BIN), "serve", "--addr", cls.addr],
            cwd=REPO_ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        deadline = time.time() + 5
        while time.time() < deadline:
            probe = subprocess.run(
                [
                    str(HYPERBOX_BIN),
                    "--server-url",
                    cls.server_url,
                    "templates",
                ],
                cwd=REPO_ROOT,
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if probe.returncode == 0:
                return
            time.sleep(0.1)
        raise RuntimeError("hyperbox server did not start")

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.terminate()
        cls.server.wait(timeout=5)
        shutil.rmtree(cls.home_dir, ignore_errors=True)

    def test_run_reuses_same_auto_session_by_default(self) -> None:
        with HyperboxClient(self.addr) as client:
            first = client.run(
                template="python:3.12",
                command="echo py-sdk-state > .py_sdk_reuse_test",
            )
            self.assertEqual(first.process.status, "succeeded")
            self.assertTrue(first.session_created)
            self.assertTrue(first.session_name)

            second = client.run(
                template="python:3.12",
                command="cat .py_sdk_reuse_test",
            )
            self.assertEqual(second.process.status, "succeeded")
            self.assertFalse(second.session_created)
            self.assertEqual(second.session_name, first.session_name)
            self.assertIn("py-sdk-state", second.stdout)

    def test_start_run_supports_detach_wait_and_logs(self) -> None:
        with HyperboxClient(self.addr) as client:
            started = client.start_run(
                create_config={"template": "python:3.12"},
                command="python3 -c 'import time; print(\"py-detached\"); time.sleep(1)'",
                reuse_auto_session=False,
            )
            self.assertEqual(started.process.status, "running")

            completed = client.wait_process(started.process.process_id, timeout_secs=5)
            self.assertEqual(completed.status, "succeeded")

            stdout = client.read_process_log(started.process.process_id, "stdout")
            self.assertIn("py-detached", stdout.contents)


if __name__ == "__main__":
    unittest.main()
