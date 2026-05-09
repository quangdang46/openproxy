"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";
import type { InputProps } from "@/types";

/**
 * MiniMax text-input — 8px rounded, 1px hairline border, 2px brand-blue-deep
 * focus ring, 40px desktop height. Error: 1px #d45656 border + matching
 * red label.
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
            <span className="material-symbols-outlined text-[18px]">{icon}</span>
          </div>
        )}
        <input
          type={type}
          placeholder={placeholder}
          value={value}
          onChange={onChange}
          disabled={disabled}
          className={cn(
            "w-full h-10 py-2.5 px-3 text-[14px] text-ink bg-canvas rounded-mini-md",
            "border border-hairline placeholder:text-steel",
            "focus:outline-none focus:border-brand-blue-deep focus:ring-0 focus:[border-width:2px] focus:px-[11px]",
            "transition-colors duration-150 ease-out disabled:opacity-50 disabled:cursor-not-allowed",
            // iOS zoom fix
            "text-[16px] sm:text-[14px]",
            icon && "pl-10",
            error &&
              "border-[color:var(--color-danger)] focus:border-[color:var(--color-danger)]",
            inputClassName
          )}
          {...props}
        />
      </div>
      {error && (
        <p className="type-body-sm text-[color:var(--color-danger)] flex items-center gap-1">
          <span className="material-symbols-outlined text-[14px]">error</span>
          {error}
        </p>
      )}
      {hint && !error && (
        <p className="type-body-sm text-slate">{hint}</p>
      )}
    </div>
  );
}
