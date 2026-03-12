# `@hyperbox/sdk`

TypeScript SDK for Hyperbox managed sandbox execution.

## What it covers

- create and destroy sandboxes
- inspect and resolve named sandboxes
- start, wait, list, and cancel managed processes
- read process logs
- run synchronous and detached commands through the same control-plane flow as the CLI
- prepare overflow sandboxes when a target sandbox is already busy

The SDK talks to the Hyperbox gRPC control plane directly. It does not shell out to the CLI.

## Install

From this repository:

```bash
cd hyperbox-ts
npm install
npm run build
```

## Example

High-level `run()` mirrors the CLI-managed process flow:

```ts
import { HyperboxClient } from "@hyperbox/sdk";

const client = new HyperboxClient("127.0.0.1:50051");
const result = await client.run({
  template: "python:3.12",
  command: "python3 -c 'print(2 + 2)'",
});
console.log(result.status, result.stdout);
await client.close();
```

Lower-level process APIs are still available when you need explicit sandbox or process lifecycle control.

## Test

```bash
cd hyperbox-ts
npm test
```
