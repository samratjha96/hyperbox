import grpc from "@grpc/grpc-js";
import protoLoader from "@grpc/proto-loader";
import { join } from "node:path";

const PROTO_PATH = join(
  import.meta.dirname,
  "..",
  "proto",
  "hyperbox",
  "v1",
  "control.proto",
);

const NETWORK_MODE_NONE = 1;
const NETWORK_MODE_ALLOWLIST = 2;
const NETWORK_MODE_FULL = 3;

type NetworkMode = "none" | "full" | "allowlist";
type ProcessDisposition =
  | "reused_existing"
  | "created_new"
  | "created_due_to_busy";
type ProcessStatus =
  | "starting"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "lost";
type LogStream = "stdout" | "stderr";

export interface SandboxInfo {
  id: string;
  template: string;
  state: string;
  createdAt: string;
}

export interface SandboxConfig {
  affinityName?: string;
  template: string;
  memoryMb: number;
  vcpuCount: number;
  timeoutSecs: number;
  env: Record<string, string>;
  workspaceDir?: string;
  network: NetworkMode;
  allowlist: string[];
}

export interface ProcessInfo {
  processId: string;
  sandboxId: string;
  requestedSandboxId?: string;
  disposition: ProcessDisposition;
  destroySandboxOnExpiry: boolean;
  command: string[];
  status: ProcessStatus;
  stdoutPath: string;
  stderrPath: string;
  backendPid?: number;
  exitCode?: number;
  startedAt: string;
  finishedAt?: string;
  expiresAt?: string;
}

export interface ProcessLogRead {
  stream: LogStream;
  offset: number;
  nextOffset: number;
  eof: boolean;
  contents: string;
}

export interface CreateSandboxOptions {
  affinityName?: string;
  template?: string;
  memoryMb?: number;
  vcpuCount?: number;
  timeoutSecs?: number;
  env?: Record<string, string>;
  workspaceDir?: string;
  network?: NetworkMode;
  allowlist?: string[];
}

export interface StartProcessOptions {
  sandboxId: string;
  command: string[];
  requestedSandboxId?: string;
  disposition?: ProcessDisposition;
  destroySandboxOnExpiry?: boolean;
}

export interface WaitProcessOptions {
  timeoutSecs?: number;
}

export interface ReadProcessLogOptions {
  offset?: number;
  limit?: number;
}

export interface PrepareRunSandboxOptions {
  sandboxId: string;
  overflowConfig: SandboxConfig;
}

export interface PreparedRunSandbox {
  sandbox: SandboxInfo;
  requestedSandboxId?: string;
  disposition: ProcessDisposition;
}

type RpcRequest = Record<string, unknown>;
type RpcCallback<T> = (err: grpc.ServiceError | null, response?: T) => void;
type RpcMethod<TRequest extends RpcRequest, TResponse> = (
  request: TRequest,
  callback: RpcCallback<TResponse>,
) => void;

type RawSandboxInfo = {
  id: string;
  template: string;
  state: string;
  created_at: string;
};

type RawSandboxConfig = {
  affinity_name: string;
  template: string;
  memory_mb: number;
  vcpu_count: number;
  timeout_secs: number;
  env: Record<string, string>;
  workspace_dir: string;
  network_mode: number;
  network_allowlist: string[];
};

type RawProcessInfo = {
  process_id: string;
  sandbox_id: string;
  requested_sandbox_id: string;
  disposition: string;
  destroy_sandbox_on_expiry: boolean;
  command: string[];
  status: string;
  stdout_path: string;
  stderr_path: string;
  backend_pid: number;
  has_backend_pid: boolean;
  exit_code: number;
  has_exit_code: boolean;
  started_at: string;
  finished_at: string;
  expires_at: string;
};

type ControlClient = grpc.Client & Record<string, RpcMethod<RpcRequest, unknown>>;

function loadControlClient(target: string): ControlClient {
  const packageDefinition = protoLoader.loadSync(PROTO_PATH, {
    keepCase: true,
    longs: String,
    enums: String,
    defaults: true,
    oneofs: true,
  });
  const loaded = grpc.loadPackageDefinition(packageDefinition) as {
    hyperbox: {
      v1: {
        HyperboxControl: grpc.ServiceClientConstructor;
      };
    };
  };
  return new loaded.hyperbox.v1.HyperboxControl(
    target,
    grpc.credentials.createInsecure(),
  ) as ControlClient;
}

function requireField<T>(value: T | undefined | null, name: string): T {
  if (value === undefined || value === null) {
    throw new Error(`missing ${name} in Hyperbox response`);
  }
  return value;
}

function camelToSnake(value: string): string {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z])([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();
}

function mapSandboxInfo(info: RawSandboxInfo): SandboxInfo {
  return {
    id: info.id,
    template: info.template,
    state: info.state.toLowerCase(),
    createdAt: info.created_at,
  };
}

function mapProcessInfo(info: RawProcessInfo): ProcessInfo {
  return {
    processId: info.process_id,
    sandboxId: info.sandbox_id,
    requestedSandboxId: info.requested_sandbox_id || undefined,
    disposition: camelToSnake(info.disposition) as ProcessDisposition,
    destroySandboxOnExpiry: info.destroy_sandbox_on_expiry,
    command: info.command,
    status: info.status.toLowerCase() as ProcessStatus,
    stdoutPath: info.stdout_path,
    stderrPath: info.stderr_path,
    backendPid: info.has_backend_pid ? info.backend_pid : undefined,
    exitCode: info.has_exit_code ? info.exit_code : undefined,
    startedAt: info.started_at,
    finishedAt: info.finished_at || undefined,
    expiresAt: info.expires_at || undefined,
  };
}

function mapNetworkMode(
  network: NetworkMode,
  allowlist: string[],
): { network_mode: number; network_allowlist: string[] } {
  switch (network) {
    case "none":
      return { network_mode: NETWORK_MODE_NONE, network_allowlist: [] };
    case "full":
      return { network_mode: NETWORK_MODE_FULL, network_allowlist: [] };
    case "allowlist":
      return { network_mode: NETWORK_MODE_ALLOWLIST, network_allowlist: allowlist };
  }
}

function mapSandboxConfig(config: CreateSandboxOptions): RawSandboxConfig {
  const network = config.network ?? "none";
  const allowlist = config.allowlist ?? [];
  const networkFields = mapNetworkMode(network, allowlist);
  return {
    affinity_name: config.affinityName ?? "",
    template: config.template ?? "python:3.12",
    memory_mb: config.memoryMb ?? 512,
    vcpu_count: config.vcpuCount ?? 1,
    timeout_secs: config.timeoutSecs ?? 60,
    env: config.env ?? {},
    workspace_dir: config.workspaceDir ?? "",
    ...networkFields,
  };
}

function mapExplicitSandboxConfig(config: SandboxConfig): RawSandboxConfig {
  const networkFields = mapNetworkMode(config.network, config.allowlist);
  return {
    affinity_name: config.affinityName ?? "",
    template: config.template,
    memory_mb: config.memoryMb,
    vcpu_count: config.vcpuCount,
    timeout_secs: config.timeoutSecs,
    env: config.env,
    workspace_dir: config.workspaceDir ?? "",
    ...networkFields,
  };
}

export class HyperboxClient {
  private readonly client: ControlClient;

  constructor(target = "127.0.0.1:50051") {
    this.client = loadControlClient(target);
  }

  async close(): Promise<void> {
    this.client.close();
  }

  private rpc<TRequest extends RpcRequest, TResponse>(
    methodName: string,
    request: TRequest,
  ): Promise<TResponse> {
    const method = this.client[methodName] as RpcMethod<TRequest, TResponse> | undefined;
    if (!method) {
      throw new Error(`unknown Hyperbox RPC: ${methodName}`);
    }
    return new Promise<TResponse>((resolve, reject) => {
      method.call(this.client, request, (error, response) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(requireField(response, methodName));
      });
    });
  }

  async listTemplates(): Promise<string[]> {
    const response = await this.rpc<
      Record<string, never>,
      { templates: string[] }
    >("ListTemplates", {});
    return response.templates;
  }

  async createSandbox(options: CreateSandboxOptions = {}): Promise<SandboxInfo> {
    const response = await this.rpc<
      { config: RawSandboxConfig },
      { info?: RawSandboxInfo }
    >("CreateSandbox", {
      config: mapSandboxConfig(options),
    });
    return mapSandboxInfo(requireField(response.info, "sandbox info"));
  }

  async destroySandbox(sandboxId: string): Promise<void> {
    await this.rpc<{ sandbox_id: string }, Record<string, never>>("DestroySandbox", {
      sandbox_id: sandboxId,
    });
  }

  async inspectSandbox(
    sandboxId: string,
  ): Promise<{ sandbox: SandboxInfo; config: SandboxConfig }> {
    const response = await this.rpc<
      { sandbox_id: string },
      { info?: RawSandboxInfo; config?: RawSandboxConfig }
    >("InspectSandbox", {
      sandbox_id: sandboxId,
    });
    const config = requireField(response.config, "sandbox config");
    return {
      sandbox: mapSandboxInfo(requireField(response.info, "sandbox info")),
      config: {
        affinityName: config.affinity_name || undefined,
        template: config.template,
        memoryMb: config.memory_mb,
        vcpuCount: config.vcpu_count,
        timeoutSecs: config.timeout_secs,
        env: config.env,
        workspaceDir: config.workspace_dir || undefined,
        network:
          config.network_mode === NETWORK_MODE_FULL
            ? "full"
            : config.network_mode === NETWORK_MODE_ALLOWLIST
              ? "allowlist"
              : "none",
        allowlist: config.network_allowlist,
      },
    };
  }

  async resolveAffinity(
    name: string,
    restoreIfNeeded = true,
  ): Promise<{ sandbox: SandboxInfo; restored: boolean }> {
    const response = await this.rpc<
      { name: string; restore_if_needed: boolean },
      { info?: RawSandboxInfo; restored: boolean }
    >("ResolveAffinity", {
      name,
      restore_if_needed: restoreIfNeeded,
    });
    return {
      sandbox: mapSandboxInfo(requireField(response.info, "sandbox info")),
      restored: response.restored,
    };
  }

  async prepareRunSandbox(options: PrepareRunSandboxOptions): Promise<PreparedRunSandbox> {
    const response = await this.rpc<
      { sandbox_id: string; overflow_config: RawSandboxConfig },
      { info?: RawSandboxInfo; requested_sandbox_id: string; disposition: string }
    >("PrepareRunSandbox", {
      sandbox_id: options.sandboxId,
      overflow_config: mapExplicitSandboxConfig(options.overflowConfig),
    });
    return {
      sandbox: mapSandboxInfo(requireField(response.info, "sandbox info")),
      requestedSandboxId: response.requested_sandbox_id || undefined,
      disposition: camelToSnake(response.disposition) as ProcessDisposition,
    };
  }

  async startProcess(options: StartProcessOptions): Promise<ProcessInfo> {
    const response = await this.rpc<
      {
        sandbox_id: string;
        command: string[];
        requested_sandbox_id: string;
        disposition: string;
        destroy_sandbox_on_expiry: boolean;
      },
      { process?: RawProcessInfo }
    >("StartProcess", {
      sandbox_id: options.sandboxId,
      command: options.command,
      requested_sandbox_id: options.requestedSandboxId ?? "",
      disposition:
        options.disposition === "created_due_to_busy"
          ? "CreatedDueToBusy"
          : options.disposition === "created_new"
            ? "CreatedNew"
            : "ReusedExisting",
      destroy_sandbox_on_expiry: options.destroySandboxOnExpiry ?? false,
    });
    return mapProcessInfo(requireField(response.process, "process"));
  }

  async getProcess(processId: string): Promise<ProcessInfo> {
    const response = await this.rpc<
      { process_id: string },
      { process?: RawProcessInfo }
    >("GetProcess", {
      process_id: processId,
    });
    return mapProcessInfo(requireField(response.process, "process"));
  }

  async listProcesses(): Promise<ProcessInfo[]> {
    const response = await this.rpc<
      Record<string, never>,
      { processes: RawProcessInfo[] }
    >("ListProcesses", {});
    return response.processes.map(mapProcessInfo);
  }

  async readProcessLog(
    processId: string,
    stream: LogStream,
    options: ReadProcessLogOptions = {},
  ): Promise<ProcessLogRead> {
    const response = await this.rpc<
      { process_id: string; stream: string; offset: number; limit: number },
      {
        stream: string;
        offset: number;
        next_offset: number;
        eof: boolean;
        contents: string;
      }
    >("ReadProcessLog", {
      process_id: processId,
      stream,
      offset: options.offset ?? 0,
      limit: options.limit ?? 8192,
    });
    return {
      stream: response.stream.toLowerCase() as LogStream,
      offset: Number(response.offset),
      nextOffset: Number(response.next_offset),
      eof: response.eof,
      contents: response.contents,
    };
  }

  async waitProcess(
    processId: string,
    options: WaitProcessOptions = {},
  ): Promise<ProcessInfo> {
    const response = await this.rpc<
      { process_id: string; timeout_secs: number },
      { process?: RawProcessInfo }
    >("WaitProcess", {
      process_id: processId,
      timeout_secs: options.timeoutSecs ?? 60,
    });
    return mapProcessInfo(requireField(response.process, "process"));
  }

  async cancelProcess(processId: string): Promise<ProcessInfo> {
    const response = await this.rpc<
      { process_id: string },
      { process?: RawProcessInfo }
    >("CancelProcess", {
      process_id: processId,
    });
    return mapProcessInfo(requireField(response.process, "process"));
  }
}
