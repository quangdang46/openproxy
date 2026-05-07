#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// Find repo root by looking for Cargo.toml (Rust project marker)
let repoRoot = path.resolve(__dirname, "..");
while (!fs.existsSync(path.join(repoRoot, "Cargo.toml")) && repoRoot !== path.dirname(repoRoot)) {
  repoRoot = path.dirname(repoRoot);
}
const rootPackage = readJson(path.join(repoRoot, "web", "package.json"));
const version = process.env.OPENPROXY_NPM_VERSION ?? rootPackage.version;
const supportedPackages = [
  ["linux", "x64"],
  ["linux", "arm64"],
  ["darwin", "x64"],
  ["darwin", "arm64"],
  ["win32", "x64"],
];

const currentOs = normalizePlatform(process.env.OPENPROXY_TARGET_OS ?? process.platform);
const currentArch = normalizeArch(process.env.OPENPROXY_TARGET_ARCH ?? process.arch);
const binaryFileName = currentOs === "win32" ? "openproxy.exe" : "openproxy";
const releaseBinary = path.join(repoRoot, "target", "release", binaryFileName);
const webRoot = path.join(repoRoot, "web");
const astroDistDir = path.join(webRoot, "dist");
const astroStaticDir = path.join(webRoot, "dist", "_astro");
const publicDir = path.join(webRoot, "public");
const distRoot = path.join(repoRoot, "dist", "npm");

assertExists(releaseBinary, "Rust release binary missing. Run `cargo build --release` first.");
assertExists(astroDistDir, "Astro dist build missing. Run `npm run build` first.");
assertExists(astroStaticDir, "Astro static assets missing. Run `npm run build` first.");

fs.rmSync(distRoot, { recursive: true, force: true });
fs.mkdirSync(distRoot, { recursive: true });

const platformPackageName = packageNameFor(currentOs, currentArch);
const platformPackageDir = path.join(distRoot, platformPackageName);
const platformBinaryDir = path.join(platformPackageDir, "bin");
const platformDashboardDir = path.join(platformPackageDir, "dashboard");
const platformStaticDir = path.join(platformDashboardDir, "_astro");

fs.mkdirSync(platformBinaryDir, { recursive: true });
fs.cpSync(releaseBinary, path.join(platformBinaryDir, binaryFileName));
if (currentOs !== "win32") {
  fs.chmodSync(path.join(platformBinaryDir, binaryFileName), 0o755);
}

fs.cpSync(astroDistDir, platformDashboardDir, { recursive: true });
if (fs.existsSync(publicDir)) {
  fs.cpSync(publicDir, path.join(platformDashboardDir, "public"), { recursive: true });
}

writeJson(path.join(platformPackageDir, "package.json"), {
  name: platformPackageName,
  version,
  description: `OpenProxy packaged runtime for ${currentOs}-${currentArch}`,
  os: [currentOs],
  cpu: [currentArch],
  files: ["bin/", "dashboard/"],
});
fs.writeFileSync(
  path.join(platformPackageDir, "README.md"),
  [
    `# ${platformPackageName}`,
    "",
    "Platform runtime bundle for the OpenProxy npm launcher.",
    "",
    `Built for \`${currentOs}-${currentArch}\`.`,
    "",
  ].join("\n"),
);

const basePackageDir = path.join(distRoot, "openproxy");
fs.mkdirSync(basePackageDir, { recursive: true });
fs.writeFileSync(path.join(basePackageDir, "index.cjs"), renderBaseLauncher(version), "utf8");
fs.chmodSync(path.join(basePackageDir, "index.cjs"), 0o755);
writeJson(path.join(basePackageDir, "package.json"), {
  name: "openproxy",
  version,
  description: "One-command launcher for the OpenProxy Rust server and packaged Astro dashboard.",
  bin: {
    openproxy: "index.cjs",
  },
  files: ["index.cjs", "README.md"],
  engines: {
    node: ">=18",
  },
  optionalDependencies: Object.fromEntries(
    supportedPackages.map(([os, arch]) => [packageNameFor(os, arch), version]),
  ),
});
fs.writeFileSync(
  path.join(basePackageDir, "README.md"),
  [
    "# openproxy",
    "",
    "Launcher package that boots the Rust backend on `4623` and the packaged Astro dashboard on `4624`.",
    "",
    "Default local install flow before publish:",
    "",
    "```bash",
    "npm install --prefix /tmp/openproxy-prefix -g --omit=optional \\",
    `  ../${platformPackageName}/${platformPackageName}-${version}.tgz \\`,
    `  ./openproxy-${version}.tgz`,
    "/tmp/openproxy-prefix/bin/openproxy",
    "```",
    "",
  ].join("\n"),
);

console.log(`Created npm packages in ${distRoot}`);
console.log(`- base: ${path.join(basePackageDir, `openproxy-${version}.tgz`)} (after npm pack)`);
console.log(`- runtime: ${path.join(platformPackageDir, `${platformPackageName}-${version}.tgz`)} (after npm pack)`);

function renderBaseLauncher(packageVersion) {
  return `#!/usr/bin/env node
const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const PACKAGE_VERSION = ${JSON.stringify(packageVersion)};
const PLATFORM_PACKAGES = ${JSON.stringify(
    Object.fromEntries(supportedPackages.map(([os, arch]) => [`${os}:${arch}`, packageNameFor(os, arch)])),
    null,
    2,
  )};
const CLI_ONLY_COMMANDS = new Set(["provider", "key", "pool", "tunnel", "route", "completion"]);

const args = process.argv.slice(2);

if (args[0] === "--version" || args[0] === "-V") {
  console.log(PACKAGE_VERSION);
  process.exit(0);
}

if (args[0] === "--help" || args[0] === "-h" || CLI_ONLY_COMMANDS.has(args[0])) {
  process.exit(runCli(resolveRuntime().binaryPath, args));
}

const runtime = resolveRuntime();
const port = readFlagValue(args, "--port") || process.env.PORT || "4623";
const host = readFlagValue(args, "--host") || process.env.HOST || "0.0.0.0";
const dataDir = readFlagValue(args, "--data-dir") || process.env.DATA_DIR;
const dashboardPort =
  process.env.DASHBOARD_SIDECAR_PORT ||
  readFlagValue(args, "--dashboard-port") ||
  "4624";
const baseUrl = process.env.BASE_URL || "http://127.0.0.1:" + port;
const dashboardUrl =
  process.env.DASHBOARD_SIDECAR_URL || "http://127.0.0.1:" + dashboardPort;

const dashboardEnv = {
  ...process.env,
  PORT: dashboardPort,
  HOSTNAME: process.env.HOSTNAME || "127.0.0.1",
  PUBLIC_BASE_URL: process.env.PUBLIC_BASE_URL || baseUrl,
};

const rustEnv = {
  ...process.env,
  PORT: port,
  HOST: host,
  BASE_URL: baseUrl,
  PUBLIC_BASE_URL: process.env.PUBLIC_BASE_URL || baseUrl,
  DASHBOARD_SIDECAR_URL: dashboardUrl,
};

if (dataDir) {
  rustEnv.DATA_DIR = dataDir;
}

console.log("OpenProxy launcher: Rust " + host + ":" + port + ", Astro 127.0.0.1:" + dashboardPort);

const dashboardChild = spawn(process.execPath, [runtime.dashboardServer], {
  cwd: runtime.dashboardCwd,
  env: dashboardEnv,
  stdio: "inherit",
});

const rustArgs = stripLauncherFlags(args);

if (!rustArgs.includes("--port")) {
  rustArgs.push("--port", port);
}
if (!rustArgs.includes("--host")) {
  rustArgs.push("--host", host);
}
if (dataDir && !rustArgs.includes("--data-dir")) {
  rustArgs.push("--data-dir", dataDir);
}

const rustChild = spawn(runtime.binaryPath, rustArgs, {
  env: rustEnv,
  stdio: "inherit",
});

wireLifecycle(dashboardChild, rustChild);

function resolveRuntime() {
  const key = process.platform + ":" + process.arch;
  const packageName = PLATFORM_PACKAGES[key];
  if (!packageName) {
    console.error("Unsupported platform: " + key);
    process.exit(1);
  }

  let packageJsonPath;
  try {
    packageJsonPath = require.resolve(packageName + "/package.json");
  } catch (error) {
    console.error("Missing optional runtime package " + packageName + ". Reinstall openproxy.");
    process.exit(1);
  }

  const packageRoot = path.dirname(packageJsonPath);
  const binaryPath = path.join(
    packageRoot,
    "bin",
    process.platform === "win32" ? "openproxy.exe" : "openproxy",
  );
  const dashboardServer = path.join(packageRoot, "dashboard", "entry.mjs");
  if (!fs.existsSync(binaryPath) || !fs.existsSync(dashboardServer)) {
    console.error("Runtime package is incomplete: " + packageName);
    process.exit(1);
  }
  return {
    binaryPath,
    dashboardServer,
    dashboardCwd: path.dirname(dashboardServer),
  };
}

function runCli(binaryPath, cliArgs) {
  const result = spawnSync(binaryPath, cliArgs, { stdio: "inherit" });
  return result.status == null ? 1 : result.status;
}

function readFlagValue(argv, flag) {
  const index = argv.indexOf(flag);
  if (index === -1) {
    return null;
  }
  return argv[index + 1] || null;
}

function stripLauncherFlags(argv) {
  const out = [];
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--dashboard-port") {
      index += 1;
      continue;
    }
    out.push(value);
  }
  return out;
}

function wireLifecycle(dashboardChild, rustChild) {
  let shuttingDown = false;
  const shutdown = (exitCode, signal) => {
    if (shuttingDown) {
      return;
    }
    shuttingDown = true;
    for (const child of [dashboardChild, rustChild]) {
      if (!child.killed) {
        child.kill(signal);
      }
    }
    setTimeout(() => process.exit(exitCode), 150);
  };

  dashboardChild.on("exit", (code, signal) => {
    if (shuttingDown) {
      return;
    }
    if (code === 0 || signal === "SIGINT" || signal === "SIGTERM") {
      shutdown(code || 0, "SIGTERM");
      return;
    }
    console.error("Dashboard sidecar exited unexpectedly (" + (signal || code) + ").");
    shutdown(code || 1, "SIGTERM");
  });

  rustChild.on("exit", (code, signal) => {
    if (shuttingDown) {
      return;
    }
    if (code === 0 || signal === "SIGINT" || signal === "SIGTERM") {
      shutdown(code || 0, "SIGTERM");
      return;
    }
    console.error("Rust backend exited unexpectedly (" + (signal || code) + ").");
    shutdown(code || 1, "SIGTERM");
  });

  process.on("SIGINT", () => shutdown(0, "SIGINT"));
  process.on("SIGTERM", () => shutdown(0, "SIGTERM"));
}
`;
}

function packageNameFor(os, arch) {
  return `openproxy-${os}-${arch}`;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function assertExists(filePath, message) {
  if (!fs.existsSync(filePath)) {
    console.error(message);
    process.exit(1);
  }
}

function normalizePlatform(platform) {
  const known = new Set(["linux", "darwin", "win32"]);
  if (!known.has(platform)) {
    console.error(`Unsupported target platform: ${platform}`);
    process.exit(1);
  }
  return platform;
}

function normalizeArch(arch) {
  const known = new Set(["x64", "arm64"]);
  if (!known.has(arch)) {
    console.error(`Unsupported target architecture: ${arch}`);
    process.exit(1);
  }
  return arch;
}
