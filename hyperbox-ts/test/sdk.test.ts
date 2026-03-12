import { after, test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";

import { HyperboxClient } from "../src/index.js";

const repoRoot = join(import.meta.dirname, "..", "..");
const hyperboxBin = join(repoRoot, "target", "debug", "hyperbox");
const serverAddr = "127.0.0.1:60161";
const serverUrl = `http://${serverAddr}`;
const homeDir = mkdtempSync(join(tmpdir(), "hyperbox-ts-test."));

mkdirSync(join(homeDir, ".hyperbox"), { recursive: true });

spawnSync("cargo", ["build", "-p", "hyperbox-cli"], {
  cwd: repoRoot,
  stdio: "inherit",
});

const server = spawn(hyperboxBin, ["serve", "--addr", serverAddr], {
  cwd: repoRoot,
  env: {
    ...process.env,
    HOME: homeDir,
    HYPERBOX_BACKEND: "local",
  },
  stdio: "ignore",
});

after(() => {
  server.kill("SIGTERM");
});

async function waitForServer(): Promise<void> {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const probe = spawnSync(
      hyperboxBin,
      ["--server-url", serverUrl, "templates"],
      {
        cwd: repoRoot,
        env: {
          ...process.env,
          HOME: homeDir,
          HYPERBOX_BACKEND: "local",
        },
      },
    );
    if (probe.status === 0) {
      return;
    }
    await delay(100);
  }
  throw new Error("hyperbox server did not start");
}

test("sdk runs and manages a detached process", async () => {
  await waitForServer();
  const client = new HyperboxClient(serverUrl.replace("http://", ""));

  const sandbox = await client.createSandbox({ template: "python:3.12" });
  const process = await client.startProcess({
    sandboxId: sandbox.id,
    command: ["/bin/sh", "-lc", "python3 -c 'import time; print(\"hi\"); time.sleep(1)'"],
  });
  assert.equal(process.status, "running");

  const waited = await client.waitProcess(process.processId, { timeoutSecs: 5 });
  assert.equal(waited.status, "succeeded");

  const logs = await client.readProcessLog(process.processId, "stdout");
  assert.match(logs.contents, /hi/);

  await client.destroySandbox(sandbox.id);
  await client.close();
});
