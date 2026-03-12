from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Iterable, Mapping

import grpc

from .v1 import control_pb2, control_pb2_grpc

NETWORK_MODE_NONE = control_pb2.NETWORK_MODE_NONE
NETWORK_MODE_ALLOWLIST = control_pb2.NETWORK_MODE_ALLOWLIST
NETWORK_MODE_FULL = control_pb2.NETWORK_MODE_FULL


@dataclass(slots=True)
class SandboxInfo:
    id: str
    template: str
    state: str
    created_at: str


@dataclass(slots=True)
class SandboxConfig:
    template: str
    memory_mb: int
    vcpu_count: int
    timeout_secs: int
    env: dict[str, str]
    network: str
    allowlist: list[str]
    workspace_dir: str | None = None
    affinity_name: str | None = None


@dataclass(slots=True)
class SandboxDetails:
    sandbox: SandboxInfo
    config: SandboxConfig


@dataclass(slots=True)
class ActiveSandbox:
    sandbox: SandboxInfo
    affinity_name: str | None


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


@dataclass(slots=True)
class ProcessInfo:
    process_id: str
    sandbox_id: str
    requested_sandbox_id: str | None
    disposition: str
    destroy_sandbox_on_expiry: bool
    command: list[str]
    status: str
    stdout_path: str
    stderr_path: str
    backend_pid: int | None
    exit_code: int | None
    started_at: str
    finished_at: str | None
    expires_at: str | None


@dataclass(slots=True)
class ProcessLogRead:
    stream: str
    offset: int
    next_offset: int
    eof: bool
    contents: str


@dataclass(slots=True)
class PreparedRunSandbox:
    sandbox: SandboxInfo
    requested_sandbox_id: str | None
    disposition: str


@dataclass(slots=True)
class StartedRun:
    process: ProcessInfo
    sandbox: SandboxInfo
    session_name: str | None
    session_created: bool


@dataclass(slots=True)
class RunResult:
    process: ProcessInfo
    sandbox: SandboxInfo
    session_name: str | None
    session_created: bool
    stdout: str
    stderr: str
    artifacts: dict[str, str]


def _require(value, field_name: str):
    if value is None:
        raise RuntimeError(f"missing {field_name} in Hyperbox response")
    return value


def _map_sandbox_info(info) -> SandboxInfo:
    return SandboxInfo(
        id=info.id,
        template=info.template,
        state=info.state.lower(),
        created_at=info.created_at,
    )


def _map_network_mode(network_mode: int) -> str:
    if network_mode == NETWORK_MODE_FULL:
        return "full"
    if network_mode == NETWORK_MODE_ALLOWLIST:
        return "allowlist"
    return "none"


def _network_fields(network: str, allowlist: Iterable[str] | None):
    mode = network.lower()
    if mode == "none":
        return NETWORK_MODE_NONE, []
    if mode == "full":
        return NETWORK_MODE_FULL, []
    if mode == "allowlist":
        return NETWORK_MODE_ALLOWLIST, list(allowlist or [])
    raise ValueError("network must be one of: none, allowlist, full")


def _map_sandbox_config(config) -> SandboxConfig:
    return SandboxConfig(
        affinity_name=config.affinity_name or None,
        template=config.template,
        memory_mb=int(config.memory_mb),
        vcpu_count=int(config.vcpu_count),
        timeout_secs=int(config.timeout_secs),
        env=dict(config.env),
        workspace_dir=config.workspace_dir or None,
        network=_map_network_mode(config.network_mode),
        allowlist=list(config.network_allowlist),
    )


def _build_sandbox_config(
    *,
    affinity_name: str | None = None,
    template: str = "python:3.12",
    memory_mb: int = 512,
    vcpu_count: int = 1,
    timeout_secs: int = 60,
    env: Mapping[str, str] | None = None,
    workspace_dir: str | None = None,
    network: str = "none",
    allowlist: Iterable[str] | None = None,
):
    network_mode, network_allowlist = _network_fields(network, allowlist)
    return control_pb2.SandboxConfig(
        affinity_name=affinity_name or "",
        template=template,
        memory_mb=memory_mb,
        vcpu_count=vcpu_count,
        timeout_secs=timeout_secs,
        env=dict(env or {}),
        workspace_dir=workspace_dir or "",
        network_mode=network_mode,
        network_allowlist=network_allowlist,
    )


def _build_partial_sandbox_config(config: Mapping[str, object] | None):
    if not config:
        return None
    return _build_sandbox_config(
        affinity_name=config.get("affinity_name") or config.get("affinityName"),
        template=str(config.get("template", "python:3.12")),
        memory_mb=int(config.get("memory_mb", config.get("memoryMb", 512))),
        vcpu_count=int(config.get("vcpu_count", config.get("vcpuCount", 1))),
        timeout_secs=int(config.get("timeout_secs", config.get("timeoutSecs", 60))),
        env=config.get("env") or {},
        workspace_dir=config.get("workspace_dir") or config.get("workspaceDir"),
        network=str(config.get("network", "none")),
        allowlist=config.get("allowlist") or [],
    )


def _map_process_info(info) -> ProcessInfo:
    return ProcessInfo(
        process_id=info.process_id,
        sandbox_id=info.sandbox_id,
        requested_sandbox_id=info.requested_sandbox_id or None,
        disposition=info.disposition.lower(),
        destroy_sandbox_on_expiry=bool(info.destroy_sandbox_on_expiry),
        command=list(info.command),
        status=info.status.lower(),
        stdout_path=info.stdout_path,
        stderr_path=info.stderr_path,
        backend_pid=int(info.backend_pid) if info.has_backend_pid else None,
        exit_code=int(info.exit_code) if info.has_exit_code else None,
        started_at=info.started_at,
        finished_at=info.finished_at or None,
        expires_at=info.expires_at or None,
    )


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

    def list_sandboxes(self, *, timeout_secs: float = 10.0) -> list[ActiveSandbox]:
        response = self._stub.ListSandboxes(
            control_pb2.ListSandboxesRequest(), timeout=timeout_secs
        )
        return [
            ActiveSandbox(
                sandbox=_map_sandbox_info(_require(sandbox.info, "sandbox info")),
                affinity_name=sandbox.affinity_name or None,
            )
            for sandbox in response.sandboxes
        ]

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
        affinity_name: str | None = None,
        template: str = "python:3.12",
        memory_mb: int = 512,
        vcpu_count: int = 1,
        timeout_secs: int = 60,
        env: Mapping[str, str] | None = None,
        workspace: str | None = None,
        network: str = "none",
        allowlist: Iterable[str] | None = None,
        rpc_timeout_secs: float = 30.0,
    ) -> SandboxInfo:
        response = self._stub.CreateSandbox(
            control_pb2.CreateSandboxRequest(
                config=_build_sandbox_config(
                    affinity_name=affinity_name,
                    template=template,
                    memory_mb=memory_mb,
                    vcpu_count=vcpu_count,
                    timeout_secs=timeout_secs,
                    env=env,
                    workspace_dir=workspace or os.getcwd(),
                    network=network,
                    allowlist=allowlist,
                )
            ),
            timeout=rpc_timeout_secs,
        )
        return _map_sandbox_info(_require(response.info, "sandbox info"))

    def inspect_sandbox(
        self, sandbox_id: str, *, timeout_secs: float = 10.0
    ) -> SandboxDetails:
        response = self._stub.InspectSandbox(
            control_pb2.InspectSandboxRequest(sandbox_id=sandbox_id),
            timeout=timeout_secs,
        )
        return SandboxDetails(
            sandbox=_map_sandbox_info(_require(response.info, "sandbox info")),
            config=_map_sandbox_config(_require(response.config, "sandbox config")),
        )

    def destroy_sandbox(self, sandbox_id: str, *, timeout_secs: float = 30.0) -> None:
        self._stub.DestroySandbox(
            control_pb2.DestroySandboxRequest(sandbox_id=sandbox_id),
            timeout=timeout_secs,
        )

    def create_snapshot(
        self,
        sandbox_id: str,
        *,
        note: str | None = None,
        timeout_secs: float = 30.0,
    ) -> tuple[str, str]:
        response = self._stub.CreateSnapshot(
            control_pb2.CreateSnapshotRequest(
                sandbox_id=sandbox_id,
                template="",
                note=note or "",
            ),
            timeout=timeout_secs,
        )
        return response.snapshot_id, response.created_at

    def restore_snapshot(
        self, snapshot_id: str, *, timeout_secs: float = 30.0
    ) -> SandboxInfo:
        response = self._stub.RestoreSnapshot(
            control_pb2.RestoreSnapshotRequest(snapshot_id=snapshot_id),
            timeout=timeout_secs,
        )
        return _map_sandbox_info(_require(response.info, "sandbox info"))

    def resolve_affinity(
        self, name: str, *, restore_if_needed: bool = True, timeout_secs: float = 30.0
    ) -> tuple[SandboxInfo, bool]:
        response = self._stub.ResolveAffinity(
            control_pb2.ResolveAffinityRequest(
                name=name,
                restore_if_needed=restore_if_needed,
            ),
            timeout=timeout_secs,
        )
        return (
            _map_sandbox_info(_require(response.info, "sandbox info")),
            bool(response.restored),
        )

    def prepare_run_sandbox(
        self,
        sandbox_id: str,
        *,
        overflow_config: Mapping[str, object],
        timeout_secs: float = 30.0,
    ) -> PreparedRunSandbox:
        response = self._stub.PrepareRunSandbox(
            control_pb2.PrepareRunSandboxRequest(
                sandbox_id=sandbox_id,
                overflow_config=_build_partial_sandbox_config(overflow_config),
            ),
            timeout=timeout_secs,
        )
        return PreparedRunSandbox(
            sandbox=_map_sandbox_info(_require(response.info, "sandbox info")),
            requested_sandbox_id=response.requested_sandbox_id or None,
            disposition=response.disposition.lower(),
        )

    def start_run(
        self,
        *,
        sandbox_id: str | None = None,
        affinity_name: str | None = None,
        create_config: Mapping[str, object] | None = None,
        reuse_auto_session: bool = False,
        ensure_commands: Iterable[str] | None = None,
        writes: Mapping[str, bytes | str] | None = None,
        command: str,
        destroy_sandbox_on_expiry: bool = False,
        timeout_secs: float = 30.0,
    ) -> StartedRun:
        response = self._stub.StartRun(
            control_pb2.StartRunRequest(
                sandbox_id=sandbox_id or "",
                affinity_name=affinity_name or "",
                create_config=_build_partial_sandbox_config(create_config),
                reuse_auto_session=reuse_auto_session,
                ensure_commands=list(ensure_commands or []),
                writes=[
                    control_pb2.RunFileWrite(
                        path=path,
                        bytes=value.encode("utf-8") if isinstance(value, str) else value,
                    )
                    for path, value in (writes or {}).items()
                ],
                command=command,
                destroy_sandbox_on_expiry=destroy_sandbox_on_expiry,
            ),
            timeout=timeout_secs,
        )
        return StartedRun(
            process=_map_process_info(_require(response.process, "process")),
            sandbox=_map_sandbox_info(_require(response.sandbox, "sandbox info")),
            session_name=response.session_name or None,
            session_created=bool(response.session_created),
        )

    def start_process(
        self,
        sandbox_id: str,
        command: Iterable[str],
        *,
        requested_sandbox_id: str | None = None,
        disposition: str = "reused_existing",
        destroy_sandbox_on_expiry: bool = False,
        timeout_secs: float = 30.0,
    ) -> ProcessInfo:
        response = self._stub.StartProcess(
            control_pb2.StartProcessRequest(
                sandbox_id=sandbox_id,
                command=list(command),
                requested_sandbox_id=requested_sandbox_id or "",
                disposition={
                    "created_due_to_busy": "CreatedDueToBusy",
                    "created_new": "CreatedNew",
                }.get(disposition, "ReusedExisting"),
                destroy_sandbox_on_expiry=destroy_sandbox_on_expiry,
            ),
            timeout=timeout_secs,
        )
        return _map_process_info(_require(response.process, "process"))

    def get_process(self, process_id: str, *, timeout_secs: float = 10.0) -> ProcessInfo:
        response = self._stub.GetProcess(
            control_pb2.GetProcessRequest(process_id=process_id),
            timeout=timeout_secs,
        )
        return _map_process_info(_require(response.process, "process"))

    def list_processes(self, *, timeout_secs: float = 10.0) -> list[ProcessInfo]:
        response = self._stub.ListProcesses(
            control_pb2.ListProcessesRequest(), timeout=timeout_secs
        )
        return [_map_process_info(process) for process in response.processes]

    def read_process_log(
        self,
        process_id: str,
        stream: str,
        *,
        offset: int = 0,
        limit: int = 1024 * 1024,
        timeout_secs: float = 30.0,
    ) -> ProcessLogRead:
        response = self._stub.ReadProcessLog(
            control_pb2.ReadProcessLogRequest(
                process_id=process_id,
                stream=stream,
                offset=offset,
                limit=limit,
            ),
            timeout=timeout_secs,
        )
        return ProcessLogRead(
            stream=response.stream,
            offset=int(response.offset),
            next_offset=int(response.next_offset),
            eof=bool(response.eof),
            contents=response.contents,
        )

    def wait_process(
        self, process_id: str, *, timeout_secs: int = 30
    ) -> ProcessInfo:
        response = self._stub.WaitProcess(
            control_pb2.WaitProcessRequest(process_id=process_id, timeout_secs=timeout_secs),
            timeout=timeout_secs + 5,
        )
        return _map_process_info(_require(response.process, "process"))

    def cancel_process(
        self, process_id: str, *, timeout_secs: float = 30.0
    ) -> ProcessInfo:
        response = self._stub.CancelProcess(
            control_pb2.CancelProcessRequest(process_id=process_id),
            timeout=timeout_secs,
        )
        return _map_process_info(_require(response.process, "process"))

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

    def run(
        self,
        *,
        sandbox_id: str | None = None,
        affinity_name: str | None = None,
        template: str = "python:3.12",
        memory_mb: int = 512,
        vcpu_count: int = 1,
        timeout_secs: int = 60,
        env: Mapping[str, str] | None = None,
        workspace_dir: str | None = None,
        network: str = "none",
        allowlist: Iterable[str] | None = None,
        ensure_commands: Iterable[str] | None = None,
        writes: Mapping[str, bytes | str] | None = None,
        reads: Iterable[str] | None = None,
        command: str,
        detach: bool = False,
        ephemeral: bool = False,
    ) -> RunResult:
        if sandbox_id and (ephemeral or affinity_name):
            raise ValueError("ephemeral and affinity_name only apply when creating a sandbox")

        create_config = None
        if not sandbox_id and not affinity_name:
            create_config = {
                "template": template,
                "memory_mb": memory_mb,
                "vcpu_count": vcpu_count,
                "timeout_secs": timeout_secs,
                "env": dict(env or {}),
                "workspace_dir": workspace_dir,
                "network": network,
                "allowlist": list(allowlist or []),
            }

        started = self.start_run(
            sandbox_id=sandbox_id,
            affinity_name=affinity_name,
            create_config=create_config,
            reuse_auto_session=not sandbox_id and not affinity_name and not ephemeral,
            ensure_commands=ensure_commands,
            writes=writes,
            command=command,
            destroy_sandbox_on_expiry=detach and ephemeral,
            timeout_secs=timeout_secs,
        )

        if detach:
            return RunResult(
                process=started.process,
                sandbox=started.sandbox,
                session_name=started.session_name,
                session_created=started.session_created,
                stdout="",
                stderr="",
                artifacts={},
            )

        completed = self.wait_process(started.process.process_id, timeout_secs=timeout_secs)
        stdout = self.read_process_log(started.process.process_id, "stdout").contents
        stderr = self.read_process_log(started.process.process_id, "stderr").contents
        artifacts: dict[str, str] = {}
        for path in reads or []:
            artifacts[path] = self.read_file(started.sandbox.id, path).decode("utf-8")

        if ephemeral and not sandbox_id and not affinity_name:
            self.destroy_sandbox(started.sandbox.id)

        return RunResult(
            process=completed,
            sandbox=started.sandbox,
            session_name=started.session_name,
            session_created=started.session_created,
            stdout=stdout,
            stderr=stderr,
            artifacts=artifacts,
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
        env: Mapping[str, str] | None = None,
        workspace: str | None = None,
        network: str = "none",
        allowlist: Iterable[str] | None = None,
        affinity_name: str | None = None,
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
        self.affinity_name = affinity_name
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
            affinity_name=self.affinity_name,
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

    def run(
        self,
        command: str,
        *,
        ensure_commands: Iterable[str] | None = None,
        writes: Mapping[str, bytes | str] | None = None,
        reads: Iterable[str] | None = None,
        detach: bool = False,
    ) -> RunResult:
        return self.client.run(
            sandbox_id=self.ensure_sandbox(),
            command=command,
            ensure_commands=ensure_commands,
            writes=writes,
            reads=reads,
            detach=detach,
            timeout_secs=self.timeout_secs,
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
