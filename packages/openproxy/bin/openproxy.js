#!/usr/bin/env node
/**
 * openproxy CLI shim
 *
 * Resolves the platform-specific binary installed via optionalDependencies
 * and execs it, forwarding all arguments and the exit code.
 *
 * Pattern matches esbuild, @biomejs/biome, swc, turbo.
 */

'use strict';

const { spawnSync } = require('node:child_process');

// platform-arch -> npm package name
const PLATFORM_PACKAGES = {
  'linux-x64': '@openprx/openproxy-linux-x64',
  'linux-arm64': '@openprx/openproxy-linux-arm64',
  'darwin-x64': '@openprx/openproxy-darwin-x64',
  'darwin-arm64': '@openprx/openproxy-darwin-arm64',
};

function fail(msg, exitCode = 1) {
  process.stderr.write(`openproxy: ${msg}\n`);
  process.exit(exitCode);
}

function platformKey() {
  return `${process.platform}-${process.arch}`;
}

function resolveBinary() {
  const key = platformKey();
  const pkg = PLATFORM_PACKAGES[key];

  if (!pkg) {
    fail(
      `unsupported platform "${key}". ` +
        `Supported: ${Object.keys(PLATFORM_PACKAGES).join(', ')}.\n` +
        `  Install from source instead: ` +
        `https://github.com/quangdang46/openproxy#building-from-source`
    );
  }

  // Try to resolve the binary path through Node's resolver.
  // The platform package's package.json points "main" at "bin/openproxy".
  let binPath;
  try {
    binPath = require.resolve(`${pkg}/bin/openproxy`);
  } catch (err) {
    fail(
      `platform package "${pkg}" is not installed.\n` +
        `  This usually means npm/pnpm skipped the optional dependency, ` +
        `or your package manager was run with --no-optional / --ignore-optional.\n` +
        `  Try reinstalling:\n` +
        `    npm install -g @openprx/openproxy --force\n` +
        `  Or install directly:\n` +
        `    npm install -g ${pkg}\n` +
        `  Original error: ${err && err.message ? err.message : err}`
    );
  }
  return binPath;
}

function main() {
  const bin = resolveBinary();
  const result = spawnSync(bin, process.argv.slice(2), {
    stdio: 'inherit',
    // Pass signals through so Ctrl+C in the user's terminal stops the server cleanly.
    windowsHide: false,
  });

  if (result.error) {
    if (result.error.code === 'ENOENT') {
      fail(`failed to exec ${bin}: file not found (was the optional dep installed correctly?)`);
    }
    fail(`failed to exec ${bin}: ${result.error.message}`);
  }

  // Forward signal-based exits (SIGINT, SIGTERM) as the conventional 128 + signum.
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return;
  }

  process.exit(result.status ?? 1);
}

main();
