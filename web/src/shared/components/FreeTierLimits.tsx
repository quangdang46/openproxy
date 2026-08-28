"use client";

import React from "react";
import type { FreeTierInfo } from "@/types";
import Badge from "./Badge";

const CREDIT_CARD_LABELS: Record<FreeTierInfo["creditCard"], string> = {
  none: "No card required",
  registration: "Registration required",
  phone: "Phone verification",
  required: "Card required",
};

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-[11px] font-medium uppercase tracking-wide text-text-muted">
        {label}
      </span>
      <span className="text-sm text-body">{children}</span>
    </div>
  );
}

export default function FreeTierLimits({ info }: { info: FreeTierInfo }) {
  if (!info || !info.accessModel) return null;

  return (
    <div className="flex flex-col gap-4 rounded-lg border border-green-500/30 bg-green-500/10 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant="success" size="sm" dot>
          Free tier
        </Badge>
        <Badge variant="default" size="sm">
          {info.accessModel}
        </Badge>
        <Badge
          variant={info.creditCard === "none" ? "success" : "warning"}
          size="sm"
        >
          {CREDIT_CARD_LABELS[info.creditCard]}
        </Badge>
        {info.productionAllowed === false && (
          <Badge variant="warning" size="sm">
            Eval/prototyping only
          </Badge>
        )}
      </div>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {info.rateLimit && <Field label="Rate limit">{info.rateLimit}</Field>}
        {info.maxContext && <Field label="Max context">{info.maxContext}</Field>}
        {typeof info.freeModels === "number" && (
          <Field label="Free models">{info.freeModels} models</Field>
        )}
      </div>

      {info.caveats && info.caveats.length > 0 && (
        <div className="flex flex-col gap-1.5">
          <span className="text-[11px] font-medium uppercase tracking-wide text-text-muted">
            Caveats
          </span>
          <ul className="flex flex-col gap-1">
            {info.caveats.map((c, i) => (
              <li
                key={i}
                className="flex items-start gap-1.5 text-xs leading-relaxed text-body"
              >
                <span className="material-symbols-outlined mt-0.5 text-[14px] text-accent-amber shrink-0">
                  warning
                </span>
                <span>{c}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {info.lastVerified && (
        <p className="text-[11px] text-text-muted">
          Limits verified {info.lastVerified}. Reported by providers and may
          change — treat as a planning guide.
          {info.source && (
            <>
              {" "}
              <a
                href={info.source}
                target="_blank"
                rel="noopener noreferrer"
                className="text-primary hover:underline"
              >
                Source
              </a>
            </>
          )}
        </p>
      )}
    </div>
  );
}
