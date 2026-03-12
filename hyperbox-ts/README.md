# `@hyperbox/sdk`

TypeScript SDK for Hyperbox managed sandbox execution.

## What it covers

- create and destroy sandboxes
- inspect and resolve named sandboxes
- start, wait, list, and cancel managed processes
- read process logs
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

```ts
import { HyperboxClient } from "@hyperbox/sdk";

const client = new HyperboxClient("127.0.0.1:50051");

const sandbox = await client.createSandbox({ template: "python:3.12" });
const process = await client.startProcess({
  sandboxId: sandbox.id,
  command: ["/bin/sh", "-lc", "python3 -c 'print(2 + 2)'"],
});

const completed = await client.waitProcess(process.processId, { timeoutSecs: 30 });
const stdout = await client.readProcessLog(process.processId, "stdout");

console.log(completed.status, stdout.contents);

await client.destroySandbox(sandbox.id);
await client.close();
```

## Test

```bash
cd hyperbox-ts
npm test
```
