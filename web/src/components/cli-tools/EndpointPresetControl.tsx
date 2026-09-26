"use client";

import { useEffect, useMemo, useState } from "react";
import type { ChangeEvent } from "react";
import type { EndpointPreset } from "./cliEndpointPresets";
import {
  deleteKeyPreset,
  deletePreset,
  readKeyPresets,
  readPresets,
  subscribeKeyPresets,
  subscribePresets,
  upsertKeyPreset,
  upsertPreset,
} from "./cliEndpointPresets";

// This control pairs an endpoint with the key that belongs to it, but the two
// live in the two canonical browser-local stores. The pair is the shared `name`;
// neither store is widened to hold the other's field.
interface Preset extends EndpointPreset {
  apiKey: string;
}

interface EndpointPresetControlProps {
  baseUrl: string;
  apiKey: string;
  onBaseUrlChange: (value: string) => void;
  onApiKeyChange: (value: string) => void;
}

function maskApiKey(apiKey: string): string {
  if (!apiKey) return "No API key";
  if (apiKey.length <= 12) return `${apiKey.slice(0, 4)}...`;
  return `${apiKey.slice(0, 8)}...${apiKey.slice(-4)}`;
}

// Join the two stores on name; an endpoint with no key preset still shows up.
function joinPresets(): Preset[] {
  const keys = new Map<string, string>(readKeyPresets().map((k) => [k.name, k.key]));
  return readPresets().map((preset) => ({ ...preset, apiKey: keys.get(preset.name) || "" }));
}

function writePair(name: string, baseUrl: string, apiKey: string): void {
  upsertPreset(baseUrl, name);
  upsertKeyPreset(apiKey, name);
}

function removePair(name: string): void {
  deletePreset(name);
  deleteKeyPreset(name);
}

export default function EndpointPresetControl({
  baseUrl,
  apiKey,
  onBaseUrlChange,
  onApiKeyChange,
}: EndpointPresetControlProps): React.ReactNode {
  const [presets, setPresets] = useState<Preset[]>(joinPresets);
  const [selectedName, setSelectedName] = useState<string>("");

  useEffect(() => {
    const sync = (): void => setPresets(joinPresets());
    const offEndpoints = subscribePresets(sync);
    const offKeys = subscribeKeyPresets(sync);
    return () => { offEndpoints(); offKeys(); };
  }, []);

  const selectedPreset = useMemo(
    () => presets.find((preset) => preset.name === selectedName) || null,
    [presets, selectedName]
  );

  const handleSelect = (name: string): void => {
    setSelectedName(name);
    const preset = presets.find((item) => item.name === name);
    if (!preset) return;
    onBaseUrlChange(preset.baseUrl);
    onApiKeyChange(preset.apiKey);
  };

  const handleSave = (): void => {
    const trimmedBaseUrl = (baseUrl || "").trim();
    const trimmedApiKey = (apiKey || "").trim();
    if (!trimmedBaseUrl || !trimmedApiKey) return;

    let defaultName = selectedPreset?.name || trimmedBaseUrl;
    try {
      defaultName = selectedPreset?.name || new URL(trimmedBaseUrl).host;
    } catch {
      defaultName = selectedPreset?.name || trimmedBaseUrl;
    }
    const name = window.prompt("Preset name", defaultName);
    if (!name?.trim()) return;

    writePair(name.trim(), trimmedBaseUrl, trimmedApiKey);
    setSelectedName(name.trim());
  };

  const handleDelete = (): void => {
    if (!selectedPreset) return;
    removePair(selectedPreset.name);
    setSelectedName("");
  };

  return (
    <div className="flex items-center gap-2">
      <span className="w-32 shrink-0 text-sm font-semibold text-text-main text-right">Preset</span>
      <span className="material-symbols-outlined text-text-muted text-[14px]">arrow_forward</span>
      <select
        value={selectedName}
        onChange={(event: ChangeEvent<HTMLSelectElement>) => handleSelect(event.target.value)}
        className="flex-1 px-2 py-1.5 bg-surface rounded text-xs border border-border focus:outline-none focus:ring-1 focus:ring-primary/50"
      >
        <option value="">Manual / current endpoint</option>
        {presets.map((preset) => (
          <option key={preset.name} value={preset.name}>
            {preset.name} - {preset.baseUrl} ({maskApiKey(preset.apiKey)})
          </option>
        ))}
      </select>
      <button
        type="button"
        onClick={handleSave}
        disabled={!baseUrl || !apiKey}
        className="px-2 py-1.5 rounded border text-xs bg-surface border-border text-text-main hover:border-primary disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
        title="Save current Base URL and API key as a browser-local preset"
      >
        Save
      </button>
      {selectedPreset && (
        <button
          type="button"
          onClick={handleDelete}
          className="p-1 text-text-muted hover:text-red-500 rounded transition-colors"
          title="Delete selected preset"
        >
          <span className="material-symbols-outlined text-[14px]">delete</span>
        </button>
      )}
    </div>
  );
}
