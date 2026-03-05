from __future__ import annotations

import json
import shlex
import subprocess
from dataclasses import dataclass
from typing import Iterable


@dataclass(slots=True)
class ExecResult:
    exit_code: int
    duration_ms: int
    stdout: str
    stderr: str
    artifacts: list[tuple[str, str]]


class Sandbox:
    def __init__(
        self,
        template: str = "python:3.12",
        memory_mb: int = 512,
        network: Iterable[str] | str | None = None,
        timeout_secs: int = 60,
        hyperbox_bin: str = "hyperbox",
    ) -> None:
        self.template = template
        self.memory_mb = memory_mb
        self.timeout_secs = timeout_secs
        self.hyperbox_bin = hyperbox_bin

        if network is None:
            self.network_mode = "none"
            self.allowlist: list[str] = []
        elif isinstance(network, str):
            mode = network.lower()
            if mode not in {"none", "full"}:
                raise ValueError("network as string must be 'none' or 'full'")
            self.network_mode = mode
            self.allowlist = []
        else:
            self.network_mode = "allowlist"
            self.allowlist = list(network)

    def __enter__(self) -> "Sandbox":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        return None

    def exec(
        self,
        command: str,
        *,
        writes: dict[str, str] | None = None,
        reads: Iterable[str] | None = None,
        timeout_secs: int | None = None,
    ) -> ExecResult:
        cmd = [
            self.hyperbox_bin,
            "run",
            "--template",
            self.template,
            "--cmd",
            command,
            "--timeout",
            str(timeout_secs or self.timeout_secs),
            "--network",
            self.network_mode,
            "--json",
        ]

        for domain in self.allowlist:
            cmd.extend(["--allow", domain])

        for path, content in (writes or {}).items():
            cmd.extend(["--write", f"{path}={content}"])

        for path in (reads or []):
            cmd.extend(["--read", path])

        proc = subprocess.run(cmd, check=False, text=True, capture_output=True)
        if proc.returncode != 0 and not proc.stdout.strip():
            raise RuntimeError(proc.stderr.strip() or f"hyperbox exited with {proc.returncode}")

        payload = json.loads(proc.stdout)
        return ExecResult(
            exit_code=int(payload["exit_code"]),
            duration_ms=int(payload["duration_ms"]),
            stdout=str(payload["stdout"]),
            stderr=str(payload["stderr"]),
            artifacts=[(str(k), str(v)) for k, v in payload.get("artifacts", [])],
        )

    def run_python(self, code: str, *, timeout_secs: int | None = None) -> ExecResult:
        command = f"python3 -c {shlex.quote(code)}"
        return self.exec(command, timeout_secs=timeout_secs)
