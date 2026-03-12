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

type RpcRequest = Record<string, unknown>;
type RpcCallback<T> = (err: grpc.ServiceError | null, response?: T) => void;
export type RpcMethod<TRequest extends RpcRequest, TResponse> = (
  request: TRequest,
  callback: RpcCallback<TResponse>,
) => void;

export type ControlClient = grpc.Client &
  Record<string, RpcMethod<RpcRequest, unknown>>;

let controlClientConstructor: grpc.ServiceClientConstructor | undefined;
export const grpcRuntime = {
  loadSync: protoLoader.loadSync.bind(protoLoader),
  loadPackageDefinition: grpc.loadPackageDefinition.bind(grpc),
  createInsecure: grpc.credentials.createInsecure.bind(grpc.credentials),
};

function getControlClientConstructor(): grpc.ServiceClientConstructor {
  if (!controlClientConstructor) {
    const packageDefinition = grpcRuntime.loadSync(PROTO_PATH, {
      keepCase: true,
      longs: String,
      enums: String,
      defaults: true,
      oneofs: true,
    });
    const loaded = grpcRuntime.loadPackageDefinition(packageDefinition) as {
      hyperbox: {
        v1: {
          HyperboxControl: grpc.ServiceClientConstructor;
        };
      };
    };
    controlClientConstructor = loaded.hyperbox.v1.HyperboxControl;
  }
  return controlClientConstructor;
}

export function loadControlClient(target: string): ControlClient {
  const Control = getControlClientConstructor();
  return new Control(
    target,
    grpcRuntime.createInsecure(),
  ) as ControlClient;
}

export function clearControlClientConstructor(): void {
  controlClientConstructor = undefined;
}
