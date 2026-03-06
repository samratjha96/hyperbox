#!/usr/bin/env python3
"""Apples-to-apples Hyperbox vs Docker benchmark harness.

Compares equivalent lifecycle and execution paths:
1) Cold end-to-end: create + exec + destroy each run.
2) Warm exec: reuse existing sandbox/container and execute command only.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import socket
import statistics
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Summary:
    runs: int
    warmup: int
    mean_ms: float
    p50_ms: float
    p95_ms: float
    min_ms: float
    max_ms: float

    def as_dict(self) -> dict:
        return {
            "runs": self.runs,
            "warmup": self.warmup,
            "mean_ms": round(self.mean_ms, 2),
            "p50_ms": round(self.p50_ms, 2),
            "p95_ms": round(self.p95_ms, 2),
            "min_ms": round(self.min_ms, 2),
            "max_ms": round(self.max_ms, 2),
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--workspace", default=os.getcwd())
    parser.add_argument("--template", default="python:3.12")
    parser.add_argument("--docker-image", default="python:3.12")
    parser.add_argument("--hyperbox-bin", default="./target/release/hyperbox")
    parser.add_argument("--server-addr", default="127.0.0.1:51051")
    parser.add_argument("--output", default="benchmarks/apples_to_apples.json")
    return parser.parse_args()


def run_checked(cmd: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    proc = subprocess.run(cmd, env=env, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n"
            f"stdout:\n{proc.stdout}\n"
            f"stderr:\n{proc.stderr}"
        )
    return proc


def wait_for_tcp(addr: str, timeout_sec: float = 10.0) -> None:
    host, port_s = addr.rsplit(":", 1)
    port = int(port_s)
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.25)
            if sock.connect_ex((host, port)) == 0:
                return
        time.sleep(0.05)
    raise TimeoutError(f"server did not start listening on {addr} within {timeout_sec}s")


def percentile(sorted_values: list[float], p: int) -> float:
    if not sorted_values:
        return 0.0
    rank = max(0, math.ceil(len(sorted_values) * p / 100) - 1)
    return sorted_values[rank]


def benchmark_cmd(cmd: list[str], runs: int, warmup: int, env: dict[str, str] | None = None) -> Summary:
    samples: list[float] = []
    for i in range(runs + warmup):
        started = time.perf_counter()
        proc = subprocess.run(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        if proc.returncode != 0:
            raise RuntimeError(f"command failed during benchmark at iter={i}: {' '.join(cmd)}")
        if i >= warmup:
            samples.append(elapsed_ms)

    ordered = sorted(samples)
    return Summary(
        runs=runs,
        warmup=warmup,
        mean_ms=statistics.fmean(samples),
        p50_ms=percentile(ordered, 50),
        p95_ms=percentile(ordered, 95),
        min_ms=min(samples),
        max_ms=max(samples),
    )


def ensure_docker_image(image: str) -> None:
    inspect = subprocess.run(
        ["docker", "image", "inspect", image],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if inspect.returncode != 0:
        run_checked(["docker", "pull", image])


def main() -> int:
    args = parse_args()
    workspace = str(Path(args.workspace).resolve())
    hyperbox_bin = str(Path(args.hyperbox_bin).resolve())
    output_path = Path(args.output)
    server_url = f"http://{args.server_addr}"

    env = os.environ.copy()
    env.setdefault("HYPERBOX_BACKEND", "auto")
    env.setdefault("HYPERBOX_APPLE_RUNTIME", "containerization")
    env.setdefault("HYPERBOX_APPLE_HELPER", f"{hyperbox_bin} apple-helper")
    env.setdefault("RUST_LOG", "error")

    ensure_docker_image(args.docker_image)

    server = subprocess.Popen(
        [hyperbox_bin, "serve", "--addr", args.server_addr],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_for_tcp(args.server_addr)

        cold_hyperbox = [
            hyperbox_bin,
            "--server-url",
            server_url,
            "run",
            "--template",
            args.template,
            "--workspace",
            workspace,
            "--cmd",
            "true",
        ]
        cold_docker = [
            "docker",
            "run",
            "--rm",
            "--pull=never",
            "--network",
            "none",
            "--workdir",
            "/workspace",
            "--volume",
            f"{workspace}:/workspace",
            args.docker_image,
            "/bin/sh",
            "-lc",
            "true",
        ]

        cold = {
            "hyperbox_run": benchmark_cmd(cold_hyperbox, args.runs, args.warmup, env=env).as_dict(),
            "docker_run": benchmark_cmd(cold_docker, args.runs, args.warmup).as_dict(),
        }

        sandbox_id = run_checked(
            [
                hyperbox_bin,
                "--server-url",
                server_url,
                "create",
                "--template",
                args.template,
                "--workspace",
                workspace,
            ],
            env=env,
        ).stdout.strip()
        if not sandbox_id:
            raise RuntimeError("failed to parse sandbox id from hyperbox create output")

        docker_name = f"hyperbox-bench-{uuid.uuid4().hex[:12]}"
        run_checked(
            [
                "docker",
                "run",
                "-d",
                "--rm",
                "--name",
                docker_name,
                "--network",
                "none",
                "--workdir",
                "/workspace",
                "--volume",
                f"{workspace}:/workspace",
                args.docker_image,
                "sleep",
                "infinity",
            ]
        )

        try:
            warm_hyperbox = [
                hyperbox_bin,
                "--server-url",
                server_url,
                "run",
                "--sandbox-id",
                sandbox_id,
                "--cmd",
                "true",
            ]
            warm_docker = ["docker", "exec", docker_name, "/bin/sh", "-lc", "true"]
            warm = {
                "hyperbox_run_existing": benchmark_cmd(
                    warm_hyperbox, args.runs, args.warmup, env=env
                ).as_dict(),
                "docker_exec": benchmark_cmd(warm_docker, args.runs, args.warmup).as_dict(),
            }
        finally:
            subprocess.run(
                [
                    hyperbox_bin,
                    "--server-url",
                    server_url,
                    "destroy",
                    "--sandbox-id",
                    sandbox_id,
                ],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            subprocess.run(
                ["docker", "rm", "-f", docker_name],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

        result = {
            "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "host": {
                "platform": sys.platform,
                "workspace": workspace,
            },
            "config": {
                "runs": args.runs,
                "warmup": args.warmup,
                "template": args.template,
                "docker_image": args.docker_image,
                "server_url": server_url,
                "network_mode": "none",
                "command": "true",
            },
            "cold_end_to_end": cold,
            "warm_exec": warm,
        }

        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result, indent=2))
        print(f"\nWrote {output_path}")
        return 0
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)


if __name__ == "__main__":
    raise SystemExit(main())
