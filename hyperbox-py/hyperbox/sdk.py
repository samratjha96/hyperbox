from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Iterable

import grpc

from .v1 import control_pb2, control_pb2_grpc


@dataclass(slots=True)
class SandboxInfo:
    id: str
    template: str
    state: str
    created_at: str


@dataclass(slots=True)
class SdkExecResult:
    exit_code: int
    duration_ms: int
    stdout: str
    stderr: str


@dataclass(slots=True)
class ServerInfo:
    server_version: str
    process_id: str
    executable_path: str
    started_at: str
    backend_requested: str
    backend_selected: str
    backend_reason: str
    apple_runtime: str
    apple_helper_argv: list[str]


@dataclass(slots=True)
class Metrics:
    creates: int
    destroys: int
    execs: int
    exec_failures: int
    p50_exec_ms: int
    p95_exec_ms: int


class HyperboxClient:
    """Thin gRPC client for Hyperbox control plane."""

    def __init__(
        self,
        target: str = "127.0.0.1:50051",
        *,
        connect_timeout_secs: float = 5.0,
    ) -> None:
        self.target = target
        self._channel = grpc.insecure_channel(target)
        grpc.channel_ready_future(self._channel).result(timeout=connect_timeout_secs)
        self._stub = control_pb2_grpc.HyperboxControlStub(self._channel)

    def close(self) -> None:
        self._channel.close()

    def __enter__(self) -> "HyperboxClient":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self.close()

    def list_templates(self, *, timeout_secs: float = 10.0) -> list[str]:
        response = self._stub.ListTemplates(
            control_pb2.ListTemplatesRequest(), timeout=timeout_secs
        )
        return list(response.templates)

    def get_server_info(self, *, timeout_secs: float = 10.0) -> ServerInfo:
        response = self._stub.GetServerInfo(
            control_pb2.ServerInfoRequest(), timeout=timeout_secs
        )
        return ServerInfo(
            server_version=response.server_version,
            process_id=response.process_id,
            executable_path=response.executable_path,
            started_at=response.started_at,
            backend_requested=response.backend_requested,
            backend_selected=response.backend_selected,
            backend_reason=response.backend_reason,
            apple_runtime=response.apple_runtime,
            apple_helper_argv=list(response.apple_helper_argv),
        )

    def get_metrics(self, *, timeout_secs: float = 10.0) -> Metrics:
        response = self._stub.GetMetrics(control_pb2.MetricsRequest(), timeout=timeout_secs)
        return Metrics(
            creates=int(response.creates),
            destroys=int(response.destroys),
            execs=int(response.execs),
            exec_failures=int(response.exec_failures),
            p50_exec_ms=int(response.p50_exec_ms),
            p95_exec_ms=int(response.p95_exec_ms),
        )

    def create_sandbox(
        self,
        *,
        template: str = "python:3.12",
        memory_mb: int = 512,
        vcpu_count: int = 1,
        timeout_secs: int = 60,
        env: dict[str, str] | None = None,
        workspace: str | None = None,
        network: str = "none",
        allowlist: Iterable[str] | None = None,
        rpc_timeout_secs: float = 30.0,
    ) -> SandboxInfo:
        network = network.lower()
        if network == "none":
            network_mode = control_pb2.NETWORK_MODE_NONE
            network_allowlist: list[str] = []
        elif network == "full":
            network_mode = control_pb2.NETWORK_MODE_FULL
            network_allowlist = []
        elif network == "allowlist":
            network_mode = control_pb2.NETWORK_MODE_ALLOWLIST
            network_allowlist = list(allowlist or [])
        else:
            raise ValueError("network must be one of: none, allowlist, full")

        response = self._stub.CreateSandbox(
            control_pb2.CreateSandboxRequest(
                config=control_pb2.SandboxConfig(
                    template=template,
                    memory_mb=memory_mb,
                    vcpu_count=vcpu_count,
                    timeout_secs=timeout_secs,
                    env=env or {},
                    workspace_dir=workspace or os.getcwd(),
                    network_mode=network_mode,
                    network_allowlist=network_allowlist,
                )
            ),
            timeout=rpc_timeout_secs,
        )
        if not response.info or not response.info.id:
            raise RuntimeError("create_sandbox returned empty info")
        return SandboxInfo(
            id=response.info.id,
            template=response.info.template,
            state=response.info.state,
            created_at=response.info.created_at,
        )

    def destroy_sandbox(self, sandbox_id: str, *, timeout_secs: float = 30.0) -> None:
        self._stub.DestroySandbox(
            control_pb2.DestroySandboxRequest(sandbox_id=sandbox_id),
            timeout=timeout_secs,
        )

    def inspect_sandbox(self, sandbox_id: str, *, timeout_secs: float = 10.0) -> SandboxInfo:
        response = self._stub.InspectSandbox(
            control_pb2.InspectSandboxRequest(sandbox_id=sandbox_id),
            timeout=timeout_secs,
        )
        if not response.info:
            raise RuntimeError("inspect_sandbox returned empty info")
        return SandboxInfo(
            id=response.info.id,
            template=response.info.template,
            state=response.info.state,
            created_at=response.info.created_at,
        )

    def exec(
        self,
        sandbox_id: str,
        command: str | Iterable[str],
        *,
        timeout_secs: int = 60,
        rpc_timeout_secs: float = 120.0,
    ) -> SdkExecResult:
        argv = (
            ["/bin/sh", "-lc", command]
            if isinstance(command, str)
            else [str(v) for v in command]
        )
        response = self._stub.Exec(
            control_pb2.ExecRequest(
                sandbox_id=sandbox_id,
                command=argv,
                timeout_secs=timeout_secs,
            ),
            timeout=rpc_timeout_secs,
        )
        return SdkExecResult(
            exit_code=int(response.exit_code),
            duration_ms=int(response.duration_ms),
            stdout=response.stdout,
            stderr=response.stderr,
        )

    def read_file(self, sandbox_id: str, path: str, *, timeout_secs: float = 30.0) -> bytes:
        response = self._stub.ReadFile(
            control_pb2.ReadFileRequest(sandbox_id=sandbox_id, path=path),
            timeout=timeout_secs,
        )
        return bytes(response.bytes)

    def write_file(
        self,
        sandbox_id: str,
        path: str,
        data: bytes | str,
        *,
        timeout_secs: float = 30.0,
    ) -> None:
        payload = data.encode("utf-8") if isinstance(data, str) else data
        self._stub.WriteFile(
            control_pb2.WriteFileRequest(sandbox_id=sandbox_id, path=path, bytes=payload),
            timeout=timeout_secs,
        )


class SandboxSession:
    """Persistent sandbox convenience wrapper on top of HyperboxClient."""

    def __init__(
        self,
        client: HyperboxClient,
        *,
        template: str = "python:3.12",
        memory_mb: int = 512,
        vcpu_count: int = 1,
        timeout_secs: int = 60,
        env: dict[str, str] | None = None,
        workspace: str | None = None,
        network: str = "none",
        allowlist: Iterable[str] | None = None,
    ) -> None:
        self.client = client
        self.template = template
        self.memory_mb = memory_mb
        self.vcpu_count = vcpu_count
        self.timeout_secs = timeout_secs
        self.env = dict(env or {})
        self.workspace = workspace
        self.network = network
        self.allowlist = list(allowlist or [])
        self._sandbox_id: str | None = None

    @property
    def sandbox_id(self) -> str | None:
        return self._sandbox_id

    def __enter__(self) -> "SandboxSession":
        self.ensure_sandbox()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self.close(suppress_errors=True)

    def ensure_sandbox(self) -> str:
        if self._sandbox_id:
            return self._sandbox_id
        info = self.client.create_sandbox(
            template=self.template,
            memory_mb=self.memory_mb,
            vcpu_count=self.vcpu_count,
            timeout_secs=self.timeout_secs,
            env=self.env,
            workspace=self.workspace,
            network=self.network,
            allowlist=self.allowlist,
        )
        self._sandbox_id = info.id
        return info.id

    def exec(
        self, command: str | Iterable[str], *, timeout_secs: int | None = None
    ) -> SdkExecResult:
        sandbox_id = self.ensure_sandbox()
        return self.client.exec(
            sandbox_id,
            command,
            timeout_secs=timeout_secs or self.timeout_secs,
        )

    def read_file(self, path: str) -> bytes:
        return self.client.read_file(self.ensure_sandbox(), path)

    def write_file(self, path: str, data: bytes | str) -> None:
        self.client.write_file(self.ensure_sandbox(), path, data)

    def close(self, *, suppress_errors: bool = False) -> None:
        if not self._sandbox_id:
            return
        sandbox_id = self._sandbox_id
        self._sandbox_id = None
        try:
            self.client.destroy_sandbox(sandbox_id)
        except Exception:
            if not suppress_errors:
                raise
