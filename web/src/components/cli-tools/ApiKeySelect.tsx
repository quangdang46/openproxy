"use client";

import { useEffect, useState } from "react";
import type { ApiKeyPreset } from "./cliEndpointPresets";
import { deleteKeyPreset, readKeyPresets, subscribeKeyPresets, upsertKeyPreset } from "./cliEndpointPresets";

const CUSTOM_VALUE = "__custom__";
const SAVE_VALUE = "__save__";
const SAVED_PREFIX = "saved:";

interface ApiKey {
  id: string;
  key: string;
}

interface ApiKeySelectProps {
  value: string;
  onChange: (value: string) => void;
  apiKeys?: ApiKey[];
  cloudEnabled?: boolean;
  className?: string;
}

export default function ApiKeySelect({
  value,
  onChange,
  apiKeys = [],
  cloudEnabled = false,
  className = "",
}: ApiKeySelectProps): React.ReactNode {
  const [savedKeys, setSavedKeys] = useState<ApiKeyPreset[]>(() => readKeyPresets());
  const [mode, setMode] = useState<string>(() => {
    if (!value) return apiKeys.length > 0 ? apiKeys[0].key : CUSTOM_VALUE;
    if (apiKeys.some((k) => k.key === value)) return value;
    return CUSTOM_VALUE;
  });
  const [customInput, setCustomInput] = useState<string>(
    !apiKeys.some((k) => k.key === value) && value !== "" ? value : ""
  );

  // A saved preset is addressed by name so two presets can hold the same key;
  // `selectedSavedKey` remembers which one is live so the value round-trip
  // through `onChange` does not drop the selection back to Custom.
  const selectedSavedKey = savedKeys.find((k) => mode === `${SAVED_PREFIX}${k.name}`)?.key ?? "";
  const liveKey = selectedSavedKey || (mode === CUSTOM_VALUE ? customInput : mode);

  // Sync internal state when value prop changes externally (e.g. from EndpointPresetControl)
  useEffect(() => {
    if (apiKeys.some((k) => k.key === value)) {
      setMode(value);
      setCustomInput("");
    } else if (value === "" || value === undefined || value === null) {
      if (apiKeys.length > 0 && mode !== apiKeys[0].key) {
        setMode(CUSTOM_VALUE);
        setCustomInput("");
      }
    } else {
      setMode(CUSTOM_VALUE);
      setCustomInput(value);
    }
  }, [value]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => subscribeKeyPresets(() => setSavedKeys(readKeyPresets())), []);

  const handleSelect = (e: React.ChangeEvent<HTMLSelectElement>): void => {
    const next = e.target.value;
    if (next === SAVE_VALUE) {
      upsertKeyPreset((liveKey || "").trim());
      return;
    }
    setMode(next);
    if (next === CUSTOM_VALUE) {
      setCustomInput("");
      onChange("");
      return;
    }
    if (next.startsWith(SAVED_PREFIX)) {
      const preset = savedKeys.find((k) => `${SAVED_PREFIX}${k.name}` === next);
      if (preset) onChange(preset.key);
      return;
    }
    onChange(next);
  };

  const handleCustomInput = (e: React.ChangeEvent<HTMLInputElement>): void => {
    const v = e.target.value;
    setCustomInput(v);
    onChange(v);
  };

  const handleDeleteSaved = (): void => {
    if (!mode.startsWith(SAVED_PREFIX)) return;
    deleteKeyPreset(mode.slice(SAVED_PREFIX.length));
    setMode(CUSTOM_VALUE);
    setCustomInput("");
    onChange("");
  };

  const isSaved = mode.startsWith(SAVED_PREFIX);
  const isCustom = mode === CUSTOM_VALUE;
  const canSave = !isSaved && (liveKey || "").trim().length > 0;

  const noKeys = apiKeys.length === 0 && mode !== CUSTOM_VALUE;

  if (noKeys && mode !== CUSTOM_VALUE) {
    return (
      <span className={`min-w-0 rounded bg-surface/40 px-2 py-2 text-xs text-text-muted sm:py-1.5 ${className}`}>
        {cloudEnabled ? "No API keys - Create one in Keys page" : "sk_openproxy (default)"}
      </span>
    );
  }

  return (
    <div className={`flex flex-col gap-1.5 ${className}`}>
      <div className="flex items-center gap-2">
        <select
          value={mode}
          onChange={handleSelect}
          className="flex-1 min-w-0 px-2 py-2 bg-surface rounded text-xs border border-border focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
        >
          {apiKeys.map((k) => (
            <option key={k.id} value={k.key}>{k.key}</option>
          ))}
          {savedKeys.map((k) => (
            <option key={`${SAVED_PREFIX}${k.name}`} value={`${SAVED_PREFIX}${k.name}`}>{k.name} - {k.key}</option>
          ))}
          <option value={CUSTOM_VALUE}>Custom...</option>
          {canSave && <option value={SAVE_VALUE}>+ Save current as...</option>}
        </select>
        {isSaved && (
          <button type="button" onClick={handleDeleteSaved} className="p-1 text-text-muted hover:text-red-500 rounded transition-colors shrink-0" title="Delete saved API key">
            <span className="material-symbols-outlined text-[14px]">delete</span>
          </button>
        )}
      </div>
      {isCustom && (
        <input
          type="text"
          value={customInput}
          onChange={handleCustomInput}
          placeholder="sk-..."
          className="w-full min-w-0 px-2 py-2 bg-surface rounded border border-border text-xs focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
        />
      )}
    </div>
  );
}
