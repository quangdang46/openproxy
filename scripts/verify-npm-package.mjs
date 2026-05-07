#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");
const rootPackage = JSON.parse(
  fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"),
);
const version = rootPackage.version;
const platformPackageName = `openproxy-${process.platform}-${process.arch}`;
const distRoot = path.join(repoRoot, "dist", "npm");
const platformPackageDir = path.join(distRoot, platformPackageName);
const basePackageDir = path.join(distRoot, "openproxy");
const prefixDir = fs.mkdtempSync(path.join(os.tmpdir(), "openproxy-prefix-"));
const dataDir = path.join(prefixDir, "data");
const healthUrl = "http://127.0.0.1:4623/health";
const landingUrl = "http://127.0.0.1:4623/landing";

runChecked("cargo", ["build", "--release"], repoRoot);
runChecked("npm", ["run", "build"], repoRoot);
runChecked("node", ["scripts/build-npm-package.mjs"], repoRoot);
runChecked("npm", ["pack"], platformPackageDir);
runChecked("npm", ["pack"], basePackageDir);

const platformTarball = path.join(
  platformPackageDir,
  `${platformPackageName}-${version}.tgz`,
);
const baseTarball = path.join(basePackageDir, `openproxy-${version}.tgz`);

runChecked(
  "npm",
  ["install", "--prefix", prefixDir, "-g", "--omit=optional", platformTarball, baseTarball],
  repoRoot,
);

fs.mkdirSync(dataDir, { recursive: true });

const binName = process.platform === "win32" ? "openproxy.cmd" : "openproxy";
const launcherPath = path.join(prefixDir, "bin", binName);
const child = spawn(launcherPath, [], {
  env: {
    ...process.env,
    DATA_DIR: dataDir,
    PORT: "4623",
    DASHBOARD_SIDECAR_PORT: "4624",
  },
  stdio: "inherit",
});

let cleanedUp = false;
const cleanup = () => {
  if (cleanedUp) {
    return;
  }
  cleanedUp = true;
  if (!child.killed) {
    child.kill("SIGTERM");
  }
};

process.on("exit", cleanup);
process.on("SIGINT", () => {
  cleanup();
  process.exit(1);
});
process.on("SIGTERM", () => {
  cleanup();
  process.exit(1);
});

await waitForUrl(healthUrl, (text) => text.includes("ok"), "health endpoint");
await waitForUrl(
  landingUrl,
  (text) => text.toLowerCase().includes("openproxy"),
  "landing page",
);

console.log(`Verified packaged launcher from ${launcherPath}`);
cleanup();

function runChecked(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

async function waitForUrl(url, predicate, label) {
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      const text = await response.text();
      if (response.ok && predicate(text)) {
        return;
      }
    } catch {
      // Retry until the stack is ready.
    }
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }

  console.error(`Timed out waiting for ${label}: ${url}`);
  cleanup();
  process.exit(1);
}
