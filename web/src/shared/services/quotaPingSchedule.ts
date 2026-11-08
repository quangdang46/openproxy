// Anchored ping schedule: fires pings at fixed local wall-clock slots instead of
// (or on top of) the reactive "ping right after reset" behaviour.
//
// With anchor 09:30, a 5h window and 3 slots/day the schedule is
// 09:30 / 14:30 / 19:30 local time — the overnight gap is what re-aligns the
// next day back onto the 09:30 anchor, since 24h is not a multiple of 5h.
//
// Port of 9router `src/shared/services/quotaPingSchedule.js` — the Rust
// scheduler (`src/server/api/quota_ping_schedule.rs`) implements the same
// model server-side; keep the two in sync.

export interface PingSchedule {
  enabled?: boolean;
  anchor?: string;
  timezone?: string;
  windowHours?: number;
  slotsPerDay?: number;
}

export interface PingSlot {
  key: string;
  index: number;
  at: number;
}

export function getLocalTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export const PING_SCHEDULE_DEFAULTS = {
  anchor: "09:30",
  timezone: getLocalTimezone(),
  windowHours: 5,
  slotsPerDay: 3,
};

// Fire a slot that came due while the process was down/busy, but never a stale one.
export const SLOT_GRACE_MS = 30 * 60 * 1000;

const HOUR_MS = 3600000;
const DAY_MS = 86400000;

function isValidTimezone(timezone: unknown): timezone is string {
  if (!timezone || typeof timezone !== "string") return false;
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: timezone });
    return true;
  } catch {
    return false;
  }
}

export function parseAnchor(anchor: unknown): { hour: number; minute: number } | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(String(anchor || "").trim());
  if (!match) return null;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  if (hour > 23 || minute > 59) return null;
  return { hour, minute };
}

export function maxSlotsPerDay(windowHours: number): number {
  return Math.max(1, Math.floor(24 / windowHours));
}

function clampNumber(value: unknown, fallback: number, min: number, max: number): number {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, parsed));
}

// Returns a fully-defaulted schedule, or null when it is absent/disabled/invalid.
export function normalizePingSchedule(raw: PingSchedule | null | undefined): Required<PingSchedule> | null {
  if (!raw || raw.enabled !== true) return null;

  const anchorParts = parseAnchor(raw.anchor) || parseAnchor(PING_SCHEDULE_DEFAULTS.anchor)!;
  const anchor = `${String(anchorParts.hour).padStart(2, "0")}:${String(anchorParts.minute).padStart(2, "0")}`;
  const timezone = isValidTimezone(raw.timezone) ? raw.timezone : PING_SCHEDULE_DEFAULTS.timezone;
  const windowHours = clampNumber(raw.windowHours, PING_SCHEDULE_DEFAULTS.windowHours, 0.5, 24);
  const slotsPerDay = Math.round(
    clampNumber(raw.slotsPerDay, PING_SCHEDULE_DEFAULTS.slotsPerDay, 1, maxSlotsPerDay(windowHours)),
  );

  return { enabled: true, anchor, timezone, windowHours, slotsPerDay };
}

function zonedParts(utcMs: number, timezone: string) {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: timezone,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).formatToParts(new Date(utcMs));

  const out: Record<string, number> = {};
  for (const part of parts) {
    if (part.type !== "literal") out[part.type] = Number(part.value);
  }
  return out;
}

function zoneOffsetMs(utcMs: number, timezone: string): number {
  const p = zonedParts(utcMs, timezone);
  return Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second) - utcMs;
}

// Wall-clock time in `timezone` → UTC epoch ms (second pass covers DST shifts).
function zonedWallClockToUtcMs(
  { year, month, day, hour, minute }: { year: number; month: number; day: number; hour: number; minute: number },
  timezone: string,
): number {
  const naive = Date.UTC(year, month - 1, day, hour, minute);
  const offset = zoneOffsetMs(naive, timezone);
  const candidate = naive - offset;
  const refinedOffset = zoneOffsetMs(candidate, timezone);
  return refinedOffset === offset ? candidate : naive - refinedOffset;
}

function slotKey(anchorDayMs: number, timezone: string, index: number): string {
  const p = zonedParts(anchorDayMs, timezone);
  const date = `${p.year}-${String(p.month).padStart(2, "0")}-${String(p.day).padStart(2, "0")}`;
  return `${date}#${index}`;
}

// All slots belonging to the anchor day that contains `anchorDayMs`.
function slotsForAnchorDay(schedule: Required<PingSchedule>, anchorDayMs: number): PingSlot[] {
  const { hour, minute } = parseAnchor(schedule.anchor)!;
  const p = zonedParts(anchorDayMs, schedule.timezone);
  const firstAt = zonedWallClockToUtcMs(
    { year: p.year, month: p.month, day: p.day, hour, minute },
    schedule.timezone,
  );

  return Array.from({ length: schedule.slotsPerDay }, (_, index) => ({
    key: slotKey(anchorDayMs, schedule.timezone, index),
    index,
    at: firstAt + index * schedule.windowHours * HOUR_MS,
  }));
}

// Slots from yesterday + today + tomorrow cover everything that can already be
// due, since a day's slots never span more than 24h from its anchor.
export function getScheduleSlots(schedule: PingSchedule | null | undefined, nowMs: number = Date.now()): PingSlot[] {
  const normalized = normalizePingSchedule(schedule);
  if (!normalized) return [];
  return [
    ...slotsForAnchorDay(normalized, nowMs - DAY_MS),
    ...slotsForAnchorDay(normalized, nowMs),
    ...slotsForAnchorDay(normalized, nowMs + DAY_MS),
  ].sort((a, b) => a.at - b.at);
}

// Most recent slot that is due and still inside the grace window.
export function getDuePingSlot(
  schedule: PingSchedule | null | undefined,
  nowMs: number = Date.now(),
  graceMs: number = SLOT_GRACE_MS,
): PingSlot | null {
  const due = getScheduleSlots(schedule, nowMs).filter(
    (slot) => slot.at <= nowMs && nowMs - slot.at <= graceMs,
  );
  return due.length ? due[due.length - 1] : null;
}

export function getNextPingSlot(
  schedule: PingSchedule | null | undefined,
  nowMs: number = Date.now(),
): PingSlot | null {
  return getScheduleSlots(schedule, nowMs).find((slot) => slot.at > nowMs) || null;
}

// "09:30 → 14:30 → 19:30" preview for the UI; window end of the last slot included by caller.
export function getScheduleSlotLabels(schedule: PingSchedule | null | undefined): string[] {
  const normalized = normalizePingSchedule(schedule);
  if (!normalized) return [];
  const { hour, minute } = parseAnchor(normalized.anchor)!;
  const startMinutes = hour * 60 + minute;

  return Array.from({ length: normalized.slotsPerDay }, (_, index) => {
    const total = Math.round(startMinutes + index * normalized.windowHours * 60) % 1440;
    return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
  });
}
