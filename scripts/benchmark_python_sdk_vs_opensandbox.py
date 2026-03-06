#!/usr/bin/env python3
"""Benchmark Hyperbox Python SDK vs OpenSandbox Python SDK."""

from __future__ import annotations

import argparse
import json
import math
import os
import socket
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path

# Import local Hyperbox Python package without installation.
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "hyperbox-py"))


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
    parser.add_argument("--runs", type=int, default=20)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--workspace", default=os.getcwd())
    parser.add_argument("--template", default="python:3.12")
    parser.add_argument("--hyperbox-bin", default="./target/release/hyperbox")
    parser.add_argument("--hyperbox-addr", default="127.0.0.1:51053")
    parser.add_argument("--opensandbox-port", type=int, default=18081)
    parser.add_argument("--output", default="benchmarks/python_sdk_vs_opensandbox.json")
    parser.add_argument("--ready-timeout-sec", type=int, default=90)
    parser.add_argument("--docker-host", default=os.getenv("DOCKER_HOST", ""))
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


def percentile(sorted_values: list[float], p: int) -> float:
    if not sorted_values:
        return 0.0
    rank = max(0, math.ceil(len(sorted_values) * p / 100) - 1)
    return sorted_values[rank]


def summarize(samples: list[float], runs: int, warmup: int) -> Summary:
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


def wait_for_tcp(addr: str, timeout_sec: float = 15.0) -> None:
    host, port_s = addr.rsplit(":", 1)
    port = int(port_s)
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.25)
            if sock.connect_ex((host, port)) == 0:
                return
        time.sleep(0.05)
    raise TimeoutError(f"service did not listen on {addr} within {timeout_sec}s")


def wait_for_http(url: str, timeout_sec: float = 30.0) -> None:
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1.0) as resp:
                if resp.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.1)
    raise TimeoutError(f"service did not become healthy at {url} within {timeout_sec}s")


def ensure_image(image: str) -> None:
    inspect = subprocess.run(
        ["docker", "image", "inspect", image],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if inspect.returncode != 0:
        run_checked(["docker", "pull", image])


def benchmark_hyperbox_sdk(args: argparse.Namespace, workspace: str) -> dict:
    from hyperbox import HyperboxClient

    hyperbox_bin = str(Path(args.hyperbox_bin).resolve())
    env = os.environ.copy()
    env.setdefault("HYPERBOX_BACKEND", "auto")
    env.setdefault("HYPERBOX_APPLE_RUNTIME", "containerization")
    env.setdefault("HYPERBOX_APPLE_HELPER", f"{hyperbox_bin} apple-helper")
    env.setdefault("RUST_LOG", "error")

    server = subprocess.Popen(
        [hyperbox_bin, "serve", "--addr", args.hyperbox_addr],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_for_tcp(args.hyperbox_addr)

        with HyperboxClient(args.hyperbox_addr) as client:
            cold_samples: list[float] = []
            for i in range(args.runs + args.warmup):
                started = time.perf_counter()
                info = client.create_sandbox(
                    template=args.template,
                    workspace=workspace,
                    network="none",
                    timeout_secs=300,
                )
                try:
                    outcome = client.exec(info.id, "true", timeout_secs=60)
                    if outcome.exit_code != 0:
                        raise RuntimeError(f"hyperbox sdk exec failed at iter={i}")
                finally:
                    client.destroy_sandbox(info.id)

                elapsed = (time.perf_counter() - started) * 1000.0
                if i >= args.warmup:
                    cold_samples.append(elapsed)

            info = client.create_sandbox(
                template=args.template,
                workspace=workspace,
                network="none",
                timeout_secs=300,
            )
            warm_samples: list[float] = []
            try:
                for i in range(args.runs + args.warmup):
                    started = time.perf_counter()
                    outcome = client.exec(info.id, "true", timeout_secs=60)
                    if outcome.exit_code != 0:
                        raise RuntimeError(f"hyperbox sdk warm exec failed at iter={i}")
                    elapsed = (time.perf_counter() - started) * 1000.0
                    if i >= args.warmup:
                        warm_samples.append(elapsed)
            finally:
                client.destroy_sandbox(info.id)

        return {
            "cold_end_to_end": summarize(cold_samples, args.runs, args.warmup).as_dict(),
            "warm_exec": summarize(warm_samples, args.runs, args.warmup).as_dict(),
        }
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)


def benchmark_opensandbox_sdk(args: argparse.Namespace) -> dict:
    from opensandbox.config.connection_sync import ConnectionConfigSync
    from opensandbox.sync.sandbox import SandboxSync

    with tempfile.TemporaryDirectory(prefix="opensandbox-bench-") as td:
        config_path = Path(td) / "opensandbox.toml"
        run_checked(
            [
                "uvx",
                "--from",
                "opensandbox-server",
                "opensandbox-server",
                "init-config",
                str(config_path),
                "--example",
                "docker",
                "--force",
            ]
        )
        config_text = config_path.read_text(encoding="utf-8")
        config_text = config_text.replace("port = 8080", f"port = {args.opensandbox_port}")
        config_path.write_text(config_text, encoding="utf-8")

        env = os.environ.copy()
        if args.docker_host:
            env["DOCKER_HOST"] = args.docker_host

        server = subprocess.Popen(
            [
                "uvx",
                "--from",
                "opensandbox-server",
                "opensandbox-server",
                "--config",
                str(config_path),
            ],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            wait_for_http(f"http://127.0.0.1:{args.opensandbox_port}/health")

            cfg = ConnectionConfigSync(
                domain=f"127.0.0.1:{args.opensandbox_port}",
                protocol="http",
            )

            cold_samples: list[float] = []
            for i in range(args.runs + args.warmup):
                started = time.perf_counter()
                sandbox = SandboxSync.create(
                    args.template,
                    entrypoint=["tail", "-f", "/dev/null"],
                    connection_config=cfg,
                    ready_timeout=timedelta(seconds=args.ready_timeout_sec),
                )
                try:
                    result = sandbox.commands.run("true")
                    if result.error:
                        raise RuntimeError(f"opensandbox command error: {result.error}")
                finally:
                    sandbox.kill()
                    sandbox.close()

                elapsed = (time.perf_counter() - started) * 1000.0
                if i >= args.warmup:
                    cold_samples.append(elapsed)

            warm_samples: list[float] = []
            sandbox = SandboxSync.create(
                args.template,
                entrypoint=["tail", "-f", "/dev/null"],
                connection_config=cfg,
                ready_timeout=timedelta(seconds=args.ready_timeout_sec),
            )
            try:
                for i in range(args.runs + args.warmup):
                    started = time.perf_counter()
                    result = sandbox.commands.run("true")
                    if result.error:
                        raise RuntimeError(f"opensandbox command error: {result.error}")
                    elapsed = (time.perf_counter() - started) * 1000.0
                    if i >= args.warmup:
                        warm_samples.append(elapsed)
            finally:
                sandbox.kill()
                sandbox.close()

            return {
                "cold_end_to_end": summarize(cold_samples, args.runs, args.warmup).as_dict(),
                "warm_exec": summarize(warm_samples, args.runs, args.warmup).as_dict(),
            }
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)


def main() -> int:
    args = parse_args()
    workspace = str(Path(args.workspace).resolve())

    ensure_image(args.template)
    ensure_image("opensandbox/execd:v1.0.6")

    hyperbox = benchmark_hyperbox_sdk(args, workspace)
    opensandbox = benchmark_opensandbox_sdk(args)

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
            "command": "true",
        },
        "hyperbox_python_sdk": hyperbox,
        "opensandbox_python_sdk": opensandbox,
    }

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))
    print(f"\nWrote {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
