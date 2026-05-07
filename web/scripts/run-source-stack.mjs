#!/usr/bin/env node

import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// Find repo root by looking for Cargo.toml (Rust project marker)
let repoRoot = path.resolve(__dirname, "..");
while (!fs.existsSync(path.join(repoRoot, "Cargo.toml")) && repoRoot !== path.dirname(repoRoot)) {
  repoRoot = path.dirname(repoRoot);
}
const webRoot = path.join(repoRoot, "web");
const args = process.argv.slice(2);

const port = readFlagValue(args, "--port") ?? process.env.PORT ?? "4623";
const dashboardPort =
  readFlagValue(args, "--dashboard-port") ??
  process.env.DASHBOARD_SIDECAR_PORT ??
  "4624";
const host = readFlagValue(args, "--host") ?? process.env.HOST ?? "0.0.0.0";
const dataDir = readFlagValue(args, "--data-dir") ?? process.env.DATA_DIR;
const baseUrl = process.env.BASE_URL ?? `http://127.0.0.1:${port}`;
const dashboardUrl =
  process.env.DASHBOARD_SIDECAR_URL ?? `http://127.0.0.1:${dashboardPort}`;
const nextBin = path.join(webRoot, "node_modules", "next", "dist", "bin", "next");

if (!fs.existsSync(nextBin)) {
  console.error("Next.js binary not found. Run `npm install` first.");
  process.exit(1);
}

const dashboardEnv = {
  ...process.env,
  PORT: dashboardPort,
  HOSTNAME: process.env.HOSTNAME ?? "127.0.0.1",
  NEXT_PUBLIC_BASE_URL: process.env.NEXT_PUBLIC_BASE_URL ?? baseUrl,
};

const rustEnv = {
  ...process.env,
  PORT: port,
  HOST: host,
  BASE_URL: baseUrl,
  NEXT_PUBLIC_BASE_URL: process.env.NEXT_PUBLIC_BASE_URL ?? baseUrl,
  DASHBOARD_SIDECAR_URL: dashboardUrl,
};

if (dataDir) {
  rustEnv.DATA_DIR = dataDir;
}

console.log(`OpenProxy source stack: Rust ${host}:${port}, Next 127.0.0.1:${dashboardPort}`);

const dashboardChild = spawn(
  process.execPath,
  [nextBin, "dev", "--webpack", "--port", dashboardPort],
  {
    cwd: webRoot,
    env: dashboardEnv,
    stdio: "inherit",
  },
);

const cargoArgs = ["run", "--"];
const serverArgs = stripLauncherFlags(args);
if (!serverArgs.includes("--port")) {
  serverArgs.push("--port", port);
}
if (!serverArgs.includes("--host")) {
  serverArgs.push("--host", host);
}
if (dataDir && !serverArgs.includes("--data-dir")) {
  serverArgs.push("--data-dir", dataDir);
}
cargoArgs.push(...serverArgs);

const rustChild = spawn("cargo", cargoArgs, {
  cwd: repoRoot,
  env: rustEnv,
  stdio: "inherit",
});

wireLifecycle([dashboardChild, rustChild]);

function wireLifecycle(children) {
  let shuttingDown = false;

  const shutdown = (exitCode = 0, signal = "SIGTERM") => {
    if (shuttingDown) {
      return;
    }
    shuttingDown = true;
    for (const child of children) {
      if (!child.killed) {
        child.kill(signal);
      }
    }
    setTimeout(() => process.exit(exitCode), 150);
  };

  for (const [name, child] of [
    ["dashboard", dashboardChild],
    ["rust", rustChild],
  ]) {
    child.on("exit", (code, signal) => {
      if (shuttingDown) {
        return;
      }
      if (code === 0 || signal === "SIGTERM" || signal === "SIGINT") {
        shutdown(code ?? 0);
        return;
      }
      console.error(`${name} exited unexpectedly (${signal ?? code}).`);
      shutdown(code ?? 1);
    });
  }

  process.on("SIGINT", () => shutdown(0, "SIGINT"));
  process.on("SIGTERM", () => shutdown(0, "SIGTERM"));
}

function readFlagValue(argv, flag) {
  const index = argv.indexOf(flag);
  if (index === -1) {
    return null;
  }
  return argv[index + 1] ?? null;
}

function stripLauncherFlags(argv) {
  const out = [];
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--dashboard-port") {
      i += 1;
      continue;
    }
    out.push(value);
  }
  return out;
}
