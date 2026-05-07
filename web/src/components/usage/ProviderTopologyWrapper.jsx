"use client";

import { lazy, Suspense } from "react";
import PropTypes from "prop-types";

// Lazy load the heavy XYFlow component
const ProviderTopology = lazy(() => import('./ProviderTopology'));

function LoadingFallback() {
  return (
    <div className="h-[320px] w-full min-w-0 rounded-lg border border-border bg-bg-subtle/30 flex items-center justify-center">
      <span className="material-symbols-outlined animate-spin text-2xl text-text-muted">progress_activity</span>
    </div>
  );
}

export default function ProviderTopologyWrapper(props) {
  return (
    <Suspense fallback={<LoadingFallback />}>
      <ProviderTopology {...props} />
    </Suspense>
  );
}

ProviderTopologyWrapper.propTypes = {
  providers: PropTypes.arrayOf(PropTypes.shape({
    id: PropTypes.string,
    provider: PropTypes.string,
    name: PropTypes.string,
  })),
  activeRequests: PropTypes.arrayOf(PropTypes.shape({
    provider: PropTypes.string,
    model: PropTypes.string,
    account: PropTypes.string,
  })),
  lastProvider: PropTypes.string,
  errorProvider: PropTypes.string,
};
