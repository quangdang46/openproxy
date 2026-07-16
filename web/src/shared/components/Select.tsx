"use client";

import { cn } from "@/shared/utils/cn";
import React, { useMemo, useState } from "react";

interface SelectOption {
  value: string;
  label: string;
}

interface SelectProps extends Omit<React.SelectHTMLAttributes<HTMLSelectElement>, 'onChange' | 'value'> {
  label?: string;
  options?: SelectOption[];
  value: string;
  onChange: (e: React.ChangeEvent<HTMLSelectElement>) => void;
  placeholder?: string;
  error?: string;
  hint?: string;
  disabled?: boolean;
  required?: boolean;
  className?: string;
  selectClassName?: string;
  /** When true, show a text filter above the native select (useful for long lists). */
  searchable?: boolean;
  searchPlaceholder?: string;
}

export default function Select({
  label,
  options = [],
  value,
  onChange,
  placeholder = "Select an option",
  error,
  hint,
  disabled = false,
  required = false,
  className,
  selectClassName,
  searchable = false,
  searchPlaceholder = "Filter options...",
  ...props
}: SelectProps) {
  const [filter, setFilter] = useState("");

  const filteredOptions = useMemo(() => {
    if (!searchable || !filter.trim()) return options;
    const q = filter.trim().toLowerCase();
    return options.filter(
      (option) =>
        option.label.toLowerCase().includes(q) ||
        option.value.toLowerCase().includes(q),
    );
  }, [options, searchable, filter]);

  // Keep the currently selected option visible even when filtered out,
  // so the select value stays valid while the user types.
  const displayOptions = useMemo(() => {
    if (!searchable || !value) return filteredOptions;
    if (filteredOptions.some((o) => o.value === value)) return filteredOptions;
    const selected = options.find((o) => o.value === value);
    return selected ? [selected, ...filteredOptions] : filteredOptions;
  }, [filteredOptions, options, searchable, value]);

  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      {label && (
        <label className="text-sm font-medium text-text-main">
          {label}
          {required && <span className="text-red-500 ml-1">*</span>}
        </label>
      )}
      {searchable && (
        <div className="relative">
          <span className="material-symbols-outlined pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-[18px] text-text-muted">
            search
          </span>
          <input
            type="text"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            disabled={disabled}
            placeholder={searchPlaceholder}
            className={cn(
              "w-full py-2 pl-9 pr-3 text-sm text-text-main",
              "bg-surface-2 border border-transparent rounded-[10px]",
              "focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40",
              "transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed",
              "text-[16px] sm:text-sm",
            )}
            aria-label={searchPlaceholder}
          />
        </div>
      )}
      <div className="relative">
        <select
          value={value}
          onChange={onChange}
          disabled={disabled}
          className={cn(
            "w-full py-2.5 px-3 pr-10 text-sm text-text-main",
            "bg-surface-2 border border-transparent rounded-[10px] appearance-none",
            "focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40",
            "transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed",
            "text-[16px] sm:text-sm",
            error && "ring-1 ring-red-500 focus:ring-2 focus:ring-red-500/40 border-red-500/40",
            selectClassName
          )}
          {...props}
        >
          <option value="" disabled>
            {placeholder}
          </option>
          {displayOptions.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
        <div className="absolute inset-y-0 right-0 flex items-center pr-3 pointer-events-none text-text-muted">
          <span className="material-symbols-outlined text-[20px]">expand_more</span>
        </div>
      </div>
      {searchable && filter.trim() && (
        <p className="text-xs text-text-muted">
          {filteredOptions.length} of {options.length} providers
        </p>
      )}
      {error && (
        <p className="text-xs text-red-500 flex items-center gap-1">
          <span className="material-symbols-outlined text-[14px]">error</span>
          {error}
        </p>
      )}
      {hint && !error && (
        <p className="text-xs text-text-muted">{hint}</p>
      )}
    </div>
  );
}
