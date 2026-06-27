"use client";

import { useEffect, useState } from "react";

const CUSTOM_VALUE = "__custom__";

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
  const isCustom = !apiKeys.some((k) => k.key === value) && value !== "";
  const [mode, setMode] = useState<string>(() => {
    if (!value) return apiKeys.length > 0 ? apiKeys[0].key : CUSTOM_VALUE;
    if (apiKeys.some((k) => k.key === value)) return value;
    return CUSTOM_VALUE;
  });
  const [customInput, setCustomInput] = useState<string>(isCustom ? value : "");

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

  const handleSelect = (e: React.ChangeEvent<HTMLSelectElement>): void => {
    const next = e.target.value;
    setMode(next);
    if (next === CUSTOM_VALUE) {
      setCustomInput("");
      onChange("");
    } else {
      onChange(next);
    }
  };

  const handleCustomInput = (e: React.ChangeEvent<HTMLInputElement>): void => {
    const v = e.target.value;
    setCustomInput(v);
    onChange(v);
  };

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
      <select
        value={mode}
        onChange={handleSelect}
        className="w-full min-w-0 px-2 py-2 bg-surface rounded text-xs border border-border focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
      >
        {apiKeys.map((k) => (
          <option key={k.id} value={k.key}>{k.key}</option>
        ))}
        <option value={CUSTOM_VALUE}>Custom...</option>
      </select>
      {mode === CUSTOM_VALUE && (
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
