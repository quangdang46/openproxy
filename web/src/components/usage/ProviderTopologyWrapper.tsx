"use client";

import { lazy, Suspense } from "react";

// Lazy load the heavy XYFlow component
const ProviderTopology = lazy(() => import('./ProviderTopology'));

interface Provider {
  id?: string;
  provider?: string;
  name?: string;
}

interface ActiveRequest {
  provider?: string;
  model?: string;
  account?: string;
}

interface ProviderTopologyWrapperProps {
  providers?: Provider[];
  activeRequests?: ActiveRequest[];
  lastProvider?: string;
  errorProvider?: string;
}

function LoadingFallback() {
  return (
    <div className="h-[320px] w-full min-w-0 rounded-lg border border-border bg-bg-subtle/30 flex items-center justify-center">
      <span className="material-symbols-outlined animate-spin text-2xl text-text-muted">progress_activity</span>
    </div>
  );
}

export default function ProviderTopologyWrapper(props: ProviderTopologyWrapperProps) {
  return (
    <Suspense fallback={<LoadingFallback />}>
      <ProviderTopology {...props} />
    </Suspense>
  );
}
