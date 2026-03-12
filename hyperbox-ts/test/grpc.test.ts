import { afterEach, mock, test } from "node:test";
import assert from "node:assert/strict";
import {
  clearControlClientConstructor,
  grpcRuntime,
  loadControlClient,
} from "../src/grpc.js";

afterEach(() => {
  mock.reset();
  clearControlClientConstructor();
});

test("loadControlClient caches the generated service constructor", () => {
  const fakeCtor = function FakeControlClient(this: { close?: () => void }) {
    this.close = () => {};
  } as unknown as new (...args: unknown[]) => { close?: () => void };

  const loadSyncMock = mock.method(grpcRuntime, "loadSync", () => {
    return {} as ReturnType<typeof grpcRuntime.loadSync>;
  });
  const packageMock = mock.method(grpcRuntime, "loadPackageDefinition", () => {
    return {
      hyperbox: {
        v1: {
          HyperboxControl: fakeCtor,
        },
      },
    } as ReturnType<typeof grpcRuntime.loadPackageDefinition>;
  });

  const first = loadControlClient("127.0.0.1:50051");
  const second = loadControlClient("127.0.0.1:50052");

  assert.equal(typeof first.close, "function");
  assert.equal(typeof second.close, "function");
  assert.equal(loadSyncMock.mock.callCount(), 1);
  assert.equal(packageMock.mock.callCount(), 1);
});
