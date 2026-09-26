"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";
import type { InputProps } from "@/types";

/**
 * Text input — 10px rounded, transparent border, 2px coral focus ring, and no
 * fixed height so the 16px mobile font grows the box instead of being crammed
 * into one. Error: 1px red ring + matching red message.
 */
export default function Input({
  label,
  type = "text",
  placeholder,
  value,
  onChange,
  error,
  hint,
  icon,
  disabled = false,
  required = false,
  className,
  inputClassName,
  ...props
}: InputProps) {
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      {label && (
        <label className="type-body-sm-medium text-ink">
          {label}
          {required && <span className="text-[color:var(--color-danger)] ml-1">*</span>}
        </label>
      )}
      <div className="relative">
        {icon && (
          <div className="absolute inset-y-0 left-0 flex items-center pl-3 pointer-events-none text-steel">
            <span className="material-symbols-outlined text-[20px]">{icon}</span>
          </div>
        )}
        <input
          type={type}
          placeholder={placeholder}
          value={value}
          onChange={onChange}
          disabled={disabled}
          className={cn(
            "w-full py-2.5 px-3 text-sm text-ink bg-surface-2 rounded-[10px]",
            "border border-transparent placeholder:text-muted-soft",
            "focus:outline-none focus:border-brand-coral focus:ring-2 focus:ring-brand-coral/25 focus:[border-width:1px]",
            "transition-colors duration-150 ease-out disabled:opacity-50 disabled:cursor-not-allowed",
            // iOS zoom fix
            "text-[16px] sm:text-sm",
            icon && "pl-10",
            error && "ring-1 ring-red-500 focus:ring-2 focus:ring-red-500/40 border-red-500/40",
            inputClassName
          )}
          {...props}
        />
      </div>
      {error && (
        <p className="text-xs text-[color:var(--color-danger)] flex items-center gap-1">
          <span className="material-symbols-outlined text-[14px]">error</span>
          {error}
        </p>
      )}
      {hint && !error && (
        <p className="text-xs text-slate">{hint}</p>
      )}
    </div>
  );
}
