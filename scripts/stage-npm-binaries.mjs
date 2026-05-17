#!/usr/bin/env node
/**
 * stage-npm-binaries.mjs
 *
 * Called by the release workflow after binaries are built and downloaded.
 *
 * Responsibilities:
 *   1. Set the `version` field in Cargo.toml and all 5 package.json files
 *      (meta + 4 platforms), and rewrite the meta package's
 *      `optionalDependencies` versions. Keeps the Rust binary's `--version`
 *      output in lockstep with the npm package version users install.
 *   2. Extract each release tarball and place the binary at
 *      packages/platform/<npm-target>/bin/openproxy with mode 0755.
 *
 * Usage:
 *   node scripts/stage-npm-binaries.mjs <version> [--artifacts <dir>]
 *
 *   <version>       Required. Semver string (e.g. "0.1.0") or git tag ("v0.1.0").
 *                   Leading "v" is stripped.
 *   --artifacts DIR Directory containing release tarballs. Defaults to "dist/".
 *                   Expected file names:
 *                     openproxy-v<VERSION>-linux-x86_64.tar.gz
 *                     openproxy-v<VERSION>-linux-aarch64.tar.gz
 *                     openproxy-v<VERSION>-macos-x86_64.tar.gz
 *                     openproxy-v<VERSION>-macos-aarch64.tar.gz
 */

import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, copyFileSync, chmodSync, readFileSync, writeFileSync, rmSync, existsSync, readdirSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import os from 'node:os';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(SCRIPT_DIR, '..');

// Release asset suffix → npm target directory under packages/platform/
const RELEASE_TO_NPM = {
  'linux-x86_64': 'linux-x64',
  'linux-aarch64': 'linux-arm64',
  'macos-x86_64': 'darwin-x64',
  'macos-aarch64': 'darwin-arm64',
};

function parseArgs(argv) {
  const args = { version: null, artifacts: 'dist' };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--artifacts') {
      args.artifacts = argv[++i];
    } else if (a.startsWith('--artifacts=')) {
      args.artifacts = a.slice('--artifacts='.length);
    } else if (a === '-h' || a === '--help') {
      console.log(readFileSync(fileURLToPath(import.meta.url), 'utf8').match(/^\/\*\*[\s\S]*?\*\//m)[0]);
      process.exit(0);
    } else if (!a.startsWith('-') && !args.version) {
      args.version = a;
    }
  }
  if (!args.version) {
    console.error('error: version argument is required');
    console.error('usage: node scripts/stage-npm-binaries.mjs <version> [--artifacts <dir>]');
    process.exit(2);
  }
  // Strip leading "v"
  args.version = args.version.replace(/^v/, '');
  // Validate semver-ish
  if (!/^\d+\.\d+\.\d+(?:[-+].+)?$/.test(args.version)) {
    console.error(`error: "${args.version}" does not look like a semver version`);
    process.exit(2);
  }
  args.artifacts = resolve(REPO_ROOT, args.artifacts);
  return args;
}

function updatePackageJson(path, mutate) {
  const raw = readFileSync(path, 'utf8');
  const json = JSON.parse(raw);
  mutate(json);
  // Preserve a final newline; 2-space indent matches the rest of the repo's JSON style.
  writeFileSync(path, JSON.stringify(json, null, 2) + '\n');
}

function setCargoVersion(cargoPath, version) {
  const raw = readFileSync(cargoPath, 'utf8');
  // Only rewrite the first `version = "..."` line inside the [package] block.
  // We anchor on the [package] header so a stray version key elsewhere
  // (e.g. inside a dependency entry rendered across lines) is never touched.
  const lines = raw.split('\n');
  let inPackage = false;
  let rewrote = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*\[package\]\s*$/.test(line)) {
      inPackage = true;
      continue;
    }
    if (inPackage && /^\s*\[/.test(line)) {
      // Entered a new table; we're done with [package].
      break;
    }
    if (inPackage && /^\s*version\s*=\s*"[^"]*"\s*$/.test(line)) {
      lines[i] = line.replace(/"[^"]*"/, `"${version}"`);
      rewrote = true;
      break;
    }
  }
  if (!rewrote) {
    throw new Error(`could not find version key in [package] block of ${cargoPath}`);
  }
  writeFileSync(cargoPath, lines.join('\n'));
}

function setVersions(version) {
  // Cargo manifest — keeps `openproxy --version` aligned with npm/GitHub release.
  const cargoPath = join(REPO_ROOT, 'Cargo.toml');
  setCargoVersion(cargoPath, version);
  console.log(`✓ Cargo.toml -> ${version}`);

  // Meta package
  const metaPath = join(REPO_ROOT, 'packages/openproxy/package.json');
  updatePackageJson(metaPath, (json) => {
    json.version = version;
    if (json.optionalDependencies) {
      for (const key of Object.keys(json.optionalDependencies)) {
        // Pin exact version to keep meta + platform packages in lockstep.
        json.optionalDependencies[key] = version;
      }
    }
  });
  console.log(`✓ packages/openproxy/package.json -> ${version}`);

  // Platform packages
  for (const npmTarget of Object.values(RELEASE_TO_NPM)) {
    const path = join(REPO_ROOT, 'packages/platform', npmTarget, 'package.json');
    updatePackageJson(path, (json) => {
      json.version = version;
    });
    console.log(`✓ packages/platform/${npmTarget}/package.json -> ${version}`);
  }
}

function extractTarball(tarballPath, destDir) {
  // Use system tar — universally available on the GitHub-hosted runners we target.
  const result = spawnSync('tar', ['-xzf', tarballPath, '-C', destDir], { stdio: 'inherit' });
  if (result.status !== 0) {
    throw new Error(`tar failed for ${tarballPath} (exit ${result.status})`);
  }
}

function findBinary(rootDir, name) {
  // The release archives contain the binary at the top level (per release.yml's
  // package step), but tolerate one level of nesting just in case.
  const candidates = [
    join(rootDir, name),
    join(rootDir, name + '.exe'),
  ];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  // Search shallow.
  for (const entry of readdirSync(rootDir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      const inner = join(rootDir, entry.name, name);
      if (existsSync(inner)) return inner;
    }
  }
  throw new Error(`binary "${name}" not found in ${rootDir}`);
}

function stageBinaries(version, artifactsDir) {
  if (!existsSync(artifactsDir)) {
    throw new Error(`artifacts directory not found: ${artifactsDir}`);
  }

  for (const [releaseSuffix, npmTarget] of Object.entries(RELEASE_TO_NPM)) {
    const tarball = join(artifactsDir, `openproxy-v${version}-${releaseSuffix}.tar.gz`);
    if (!existsSync(tarball)) {
      throw new Error(`missing release artifact: ${tarball}`);
    }

    const tmp = mkdtempSync(join(os.tmpdir(), `openproxy-stage-${npmTarget}-`));
    try {
      extractTarball(tarball, tmp);
      const binPath = findBinary(tmp, 'openproxy');

      const destDir = join(REPO_ROOT, 'packages/platform', npmTarget, 'bin');
      mkdirSync(destDir, { recursive: true });
      const dest = join(destDir, 'openproxy');
      copyFileSync(binPath, dest);
      chmodSync(dest, 0o755);
      console.log(`✓ staged ${releaseSuffix} -> packages/platform/${npmTarget}/bin/openproxy`);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  console.log(`staging openproxy v${args.version}`);
  console.log(`  repo root: ${REPO_ROOT}`);
  console.log(`  artifacts: ${args.artifacts}`);
  console.log('');

  setVersions(args.version);
  console.log('');
  stageBinaries(args.version, args.artifacts);

  console.log('');
  console.log('done. Next steps in CI:');
  console.log('  1. publish each packages/platform/*/');
  console.log('  2. wait for npm registry propagation');
  console.log('  3. publish packages/openproxy/');
}

main();
