#!/usr/bin/env node

import { spawn } from 'child_process';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.join(__dirname, '../..');

// Environment variables for Rust backend
const rustEnv = {
  ...process.env,
  DATA_DIR: process.env.DATA_DIR || '/tmp/openproxy',
  BASE_URL: process.env.BASE_URL || 'http://127.0.0.1:4623',
  NEXT_PUBLIC_BASE_URL: process.env.NEXT_PUBLIC_BASE_URL || 'http://127.0.0.1:4623',
  DASHBOARD_SIDECAR_URL: process.env.DASHBOARD_SIDECAR_URL || 'http://127.0.0.1:4624',
};

// Environment variables for Next.js
const nextEnv = {
  ...process.env,
  NEXT_PUBLIC_BASE_URL: process.env.NEXT_PUBLIC_BASE_URL || 'http://127.0.0.1:4623',
};

console.log('🚀 Starting 9Router development stack...');
console.log('📦 Rust backend: http://127.0.0.1:4623');
console.log('🎨 Next.js dashboard: http://127.0.0.1:4624');
console.log('');

// Start Rust backend
const rustProcess = spawn('cargo', ['run', '--', '--port', '4623'], {
  cwd: rootDir,
  env: rustEnv,
  stdio: 'inherit',
});

rustProcess.on('error', (err) => {
  console.error('❌ Failed to start Rust backend:', err.message);
  process.exit(1);
});

// Wait a bit for Rust to start, then start Next.js
setTimeout(() => {
  console.log('🎨 Starting Next.js dashboard...\n');
  
  const nextProcess = spawn('npm', ['run', 'dev'], {
    cwd: path.join(rootDir, 'web'),
    env: nextEnv,
    stdio: 'inherit',
  });

  nextProcess.on('error', (err) => {
    console.error('❌ Failed to start Next.js dashboard:', err.message);
    rustProcess.kill();
    process.exit(1);
  });

  // Handle shutdown
  const cleanup = () => {
    console.log('\n🛑 Shutting down...');
    nextProcess.kill();
    rustProcess.kill();
    process.exit(0);
  };

  process.on('SIGINT', cleanup);
  process.on('SIGTERM', cleanup);

  nextProcess.on('exit', (code) => {
    console.log(`Next.js exited with code ${code}`);
    rustProcess.kill();
    process.exit(code);
  });

}, 2000);

rustProcess.on('exit', (code) => {
  console.log(`Rust backend exited with code ${code}`);
  process.exit(code);
});
