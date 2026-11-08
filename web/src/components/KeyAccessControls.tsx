"use client";

// Per-API-key access control on the Endpoint page — port of 9router
// `endpoint/components/KeyAccessControls.js`.
// "Restrict access" toggle per key; when on, the key may call only the listed
// combos/models, shown as removable badges. Entries are picked with the shared
// ModelSelectModal (combos first, then provider models).
import { useState } from "react";
import { ModelSelectModal, Toggle } from "@/shared/components";

export interface KeyAccess {
  restricted: boolean;
  allow: string[];
}

interface ApiKeyAccessProps {
  id: string;
  name: string;
  access?: KeyAccess;
}

interface KeyAccessControlsProps {
  apiKey: ApiKeyAccessProps;
  onChange: (access: KeyAccess) => void;
  onRequestRestrict: () => void;
}

interface ActiveProvider {
  provider: string;
  [key: string]: unknown;
}

interface ModelLike {
  value?: string;
}

export default function KeyAccessControls({
  apiKey,
  onChange,
  onRequestRestrict,
}: KeyAccessControlsProps) {
  const access = apiKey.access || { restricted: false, allow: [] };
  const [pickerOpen, setPickerOpen] = useState(false);
  const [activeProviders, setActiveProviders] = useState<ActiveProvider[]>([]);

  const openPicker = async () => {
    setPickerOpen(true);
    // Same source the combo editor feeds the picker with. ModelSelectModal
    // fetches combos and model aliases itself; only provider connections are
    // passed in.
    try {
      const providersRes = await fetch("/api/providers");
      if (providersRes.ok) {
        const data = await providersRes.json();
        setActiveProviders(data.connections || []);
      }
    } catch (error) {
      console.log("Error loading picker data:", error);
    }
  };

  const setAllow = (allow: string[]) => onChange({ restricted: true, allow });
  const add = (model: ModelLike) => {
    const value = model?.value;
    if (
      value &&
      !access.allow.some((e) => e.toLowerCase() === value.toLowerCase())
    ) {
      setAllow([...access.allow, value]);
    }
  };
  const remove = (value: string) =>
    setAllow(access.allow.filter((e) => e !== value));

  return (
    <div className="mt-2" data-testid="key-access">
      <Toggle
        size="sm"
        checked={access.restricted}
        label="Restrict access"
        onChange={(checked: boolean) =>
          checked
            ? onRequestRestrict()
            : onChange({ restricted: false, allow: access.allow })
        }
      />
      {access.restricted && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {access.allow.length === 0 && (
            <span className="text-xs text-orange-500">
              No combos or models allowed — this key can call nothing.
            </span>
          )}
          {access.allow.map((entry) => (
            <span
              key={entry}
              className="flex items-center gap-1 rounded-xl border border-primary/30 bg-primary/10 px-2 py-0.5 font-mono text-xs text-text-main"
            >
              {entry}
              <button
                onClick={() => remove(entry)}
                className="rounded text-text-muted hover:text-red-500"
                title={`Remove ${entry}`}
                aria-label={`Remove ${entry}`}
              >
                <span
                  className="material-symbols-outlined leading-none"
                  style={{ fontSize: "12px" }}
                >
                  close
                </span>
              </button>
            </span>
          ))}
          <button
            onClick={openPicker}
            className="flex items-center gap-0.5 rounded-xl border border-dashed border-black/10 px-2 py-0.5 text-xs font-medium text-primary hover:border-primary/50 dark:border-white/10"
          >
            <span
              className="material-symbols-outlined leading-none"
              style={{ fontSize: "14px" }}
            >
              add
            </span>
            Add combo or model
          </button>
        </div>
      )}
      {pickerOpen && (
        <ModelSelectModal
          isOpen={pickerOpen}
          onClose={() => setPickerOpen(false)}
          onSelect={add}
          onDeselect={(model: ModelLike) =>
            remove(
              access.allow.find(
                (e) => e.toLowerCase() === String(model?.value).toLowerCase(),
              ) || "",
            )
          }
          activeProviders={activeProviders}
          title={`Allowed for "${apiKey.name}"`}
          addedModelValues={access.allow}
          closeOnSelect={false}
        />
      )}
    </div>
  );
}
