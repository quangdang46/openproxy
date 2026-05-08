import { EventEmitter } from "events";
import { CONSOLE_LOG_CONFIG } from "@/shared/constants/config";

const consoleLevels = ["log", "info", "warn", "error", "debug"];

interface ConsoleLogState {
  logs: string[];
  patched: boolean;
  originals: Record<string, any>;
  emitter: EventEmitter;
}

if (!(global as any)._consoleLogBufferState) {
  (global as any)._consoleLogBufferState = {
    logs: [],
    patched: false,
    originals: {},
    emitter: new EventEmitter(),
  };
  (global as any)._consoleLogBufferState.emitter.setMaxListeners(50);
}

const state = (global as any)._consoleLogBufferState as ConsoleLogState;

// Ensure emitter exists (handles hot reload with stale global)
if (!state.emitter) {
  state.emitter = new EventEmitter();
  state.emitter.setMaxListeners(50);
}

function toLogLine(level: string, args: any[]): string {
  return args.map(formatArg).join(" ");
}

// Strip ANSI escape codes so terminal colors don't bleed into UI
const ANSI_RE = /\x1b\[[0-9;]*m/g;

function stripAnsi(str: string): string {
  return str.replace(ANSI_RE, "");
}

function formatArg(arg: any): string {
  if (typeof arg === "string") return stripAnsi(arg);
  if (arg instanceof Error) return stripAnsi(arg.stack || arg.message || String(arg));
  try {
    return stripAnsi(JSON.stringify(arg));
  } catch {
    return stripAnsi(String(arg));
  }
}

function appendLine(line: string): void {
  state.logs.push(line);
  const maxLines = CONSOLE_LOG_CONFIG.maxLines;
  if (state.logs.length > maxLines) {
    state.logs = state.logs.slice(-maxLines);
  }
  state.emitter.emit("line", line);
}

export function initConsoleLogCapture(): void {
  if (state.patched) return;

  for (const level of consoleLevels) {
    state.originals[level] = (console as any)[level];
    (console as any)[level] = (...args: any[]) => {
      appendLine(toLogLine(level, args));
      state.originals[level](...args);
    };
  }

  state.patched = true;
}

export function getConsoleLogs(): string[] {
  return state.logs;
}

export function clearConsoleLogs(): void {
  state.logs = [];
  state.emitter.emit("clear");
}

export function getConsoleEmitter(): EventEmitter {
  return state.emitter;
}
