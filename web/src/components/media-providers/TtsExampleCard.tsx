"use client";

import { useState, useEffect } from "react";
import { Card } from "@/shared/components";
import { getProviderAlias } from "@/shared/constants/providers";
import { getModelsByProviderId } from "@/shared/constants/models";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import { Row, getModelKind } from "./exampleShared";
import React from "react";

const DEFAULT_TTS_RESPONSE_EXAMPLE = `// Audio will appear here after running.
// Example JSON response (response_format=json):
{
  "format": "mp3",
  "audio": "//NExAANaAIIAUAAANNNNNNNN..." // base64 encoded MP3
}`;

export function TtsExampleCard({ providerId }: { providerId: string }) {
  const providerAlias = getProviderAlias(providerId);

  const ttsModels = getModelsByProviderId(providerId).filter((m: any) => getModelKind(m) === "tts");
  const hasModelSelector = ttsModels.length > 0;

  const [selectedModel, setSelectedModel] = useState(ttsModels[0]?.id || "");
  const [input, setInput] = useState("Hello, this is a text to speech test.");
  const [apiKey, setApiKey] = useState("");
  const [useTunnel, setUseTunnel] = useState(false);
  const [localEndpoint, setLocalEndpoint] = useState("");
  const [tunnelEndpoint, setTunnelEndpoint] = useState("");
  const [responseFormat, setResponseFormat] = useState("mp3");
  const [audioUrl, setAudioUrl] = useState("");
  const [jsonResponse, setJsonResponse] = useState<any>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState("");
  const [latency, setLatency] = useState<number | null>(null);
  const { copied: copiedCurl, copy: copyCurl } = useCopyToClipboard();

  useEffect(() => {
    setLocalEndpoint(window.location.origin);
    fetch("/api/keys")
      .then((r) => r.json())
      .then((d: any) => { setApiKey((d.keys || []).find((k: any) => k.isActive !== false)?.key || ""); })
      .catch(() => {});
    fetch("/api/tunnel/status")
      .then((r) => r.json())
      .then((d: any) => { if (d.publicUrl) setTunnelEndpoint(d.publicUrl); })
      .catch(() => {});
  }, [providerId]);

  const endpoint = useTunnel ? tunnelEndpoint : localEndpoint;
  const modelFull = selectedModel ? `${providerAlias}/${selectedModel}` : providerAlias;

  const ttsBody: Record<string, any> = { model: modelFull, input };

  const curlSnippet = `curl -X POST ${endpoint}/v1/audio/speech${responseFormat === "json" ? "?response_format=json" : ""} \\
  -H "Content-Type: application/json" \\
  -H "Authorization: Bearer ${apiKey || "YOUR_KEY"}" \\
  -d '${JSON.stringify(ttsBody)}' \\
  ${responseFormat === "json" ? "" : "--output speech.mp3"}`;

  const handleRun = async () => {
    if (!input.trim() || !modelFull) return;
    setRunning(true);
    setError("");
    setAudioUrl("");
    setJsonResponse(null);
    const start = Date.now();
    try {
      const headers: Record<string, string> = { "Content-Type": "application/json" };
      if (apiKey) headers["Authorization"] = `Bearer ${apiKey}`;
      const url = `/api/v1/audio/speech${responseFormat === "json" ? "?response_format=json" : ""}`;
      const res = await fetch(url, { method: "POST", headers, body: JSON.stringify({ ...ttsBody, input: input.trim() }) });
      setLatency(Date.now() - start);
      if (!res.ok) {
        const d = await res.json().catch(() => ({}));
        setError(d?.error?.message || d?.error || `HTTP ${res.status}`);
        return;
      }

      if (responseFormat === "json") {
        const data = await res.json();
        setJsonResponse(data);
        const audioBlob = await fetch(`data:audio/mp3;base64,${data.audio}`).then((r) => r.blob());
        setAudioUrl(URL.createObjectURL(audioBlob));
      } else {
        const blob = await res.blob();
        setAudioUrl(URL.createObjectURL(blob));
      }
    } catch (e: any) {
      setError(e.message || "Network error");
    } finally {
      setRunning(false);
    }
  };

  return (
    <Card>
      <h2 className="text-lg font-semibold mb-4">Example</h2>
      <div className="flex flex-col gap-2.5">
        <Row label="Endpoint">
          <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
            <span className="w-full min-w-0 flex-1 px-3 py-1.5 text-sm font-mono text-ink bg-surface-card border border-hairline-soft rounded-mini-md truncate">
              {endpoint}/v1/audio/speech
            </span>
            {tunnelEndpoint && (
              <button
                onClick={() => setUseTunnel((v) => !v)}
                title={useTunnel ? "Using tunnel" : "Using local"}
                className={`flex items-center gap-1 text-xs px-2 py-1.5 rounded-mini-md border shrink-0 transition-colors ${
                  useTunnel ? "border-brand-coral/40 bg-brand-coral/10 text-brand-coral" : "border-hairline text-text-muted hover:text-brand-coral"
                }`}
              >
                <span className="material-symbols-outlined text-[14px]">wifi_tethering</span>
                Tunnel
              </button>
            )}
          </div>
        </Row>

        <Row label="API Key">
          <span className="px-3 py-1.5 text-sm font-mono text-ink bg-surface-card border border-hairline-soft rounded-mini-md truncate block">
            {apiKey ? `${apiKey.slice(0, 8)}${"•".repeat(Math.min(20, apiKey.length - 8))}` : <span className="text-text-muted italic">No key configured</span>}
          </span>
        </Row>

        {hasModelSelector && (
          <Row label="Model">
            <select
              value={selectedModel}
              onChange={(e) => setSelectedModel(e.target.value)}
              className="w-full px-3 py-1.5 text-sm border border-hairline rounded-mini-md bg-canvas focus:outline-none focus:border-brand-coral"
            >
              {ttsModels.map((m: any) => (
                <option key={m.id} value={m.id}>{m.name || m.id}</option>
              ))}
            </select>
          </Row>
        )}

        <Row label="Input">
          <div className="relative">
            <input
              value={input}
              onChange={(e) => setInput(e.target.value)}
              className="w-full px-3 py-1.5 pr-7 text-sm border border-hairline rounded-mini-md bg-canvas focus:outline-none focus:border-brand-coral"
            />
            {input && (
              <button
                type="button"
                onClick={() => setInput("")}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-text-muted hover:text-brand-coral transition-colors"
              >
                <span className="material-symbols-outlined text-[14px]">close</span>
              </button>
            )}
          </div>
        </Row>

        <Row label="Output Format">
          <select
            value={responseFormat}
            onChange={(e) => setResponseFormat(e.target.value)}
            className="w-full px-3 py-1.5 text-sm border border-hairline rounded-mini-md bg-canvas focus:outline-none focus:border-brand-coral"
          >
            <option value="mp3">MP3 (Binary)</option>
            <option value="json">JSON (Base64)</option>
          </select>
        </Row>

        <div className="mt-1">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between mb-1.5">
            <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">Request</span>
            <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
              <button
                onClick={() => copyCurl(curlSnippet)}
                className="inline-flex items-center gap-1 text-xs text-text-muted hover:text-brand-coral transition-colors"
              >
                <span className="material-symbols-outlined text-[14px]">{copiedCurl ? "check" : "content_copy"}</span>
                {copiedCurl ? "Copied" : "Copy"}
              </button>
              <button
                onClick={handleRun}
                disabled={running || !input.trim() || !modelFull}
                className="flex w-full sm:w-auto items-center justify-center gap-1.5 px-3 py-1 rounded-mini-md bg-brand-coral text-white text-xs font-medium hover:bg-brand-coral/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <span className="material-symbols-outlined text-[14px]" style={running ? { animation: "spin 1s linear infinite" } : undefined}>
                  play_arrow
                </span>
                {running ? "Generating..." : "Run"}
              </button>
            </div>
          </div>
          <pre className="bg-surface-card border border-hairline-soft rounded-mini-lg px-3 py-2.5 text-xs font-mono text-ink overflow-x-auto whitespace-pre-wrap break-all">{curlSnippet}</pre>
        </div>

        {error && <p className="text-xs text-red-500 break-words">{error}</p>}

        {audioUrl ? (
          <div>
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between mb-1.5">
              <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">
                Response {latency && <span className="font-normal normal-case">&#9889; {latency}ms</span>}
              </span>
              <a href={audioUrl} download="speech.mp3" className="inline-flex items-center gap-1 text-xs text-text-muted hover:text-brand-coral transition-colors">
                <span className="material-symbols-outlined text-[14px]">download</span>
                Download
              </a>
            </div>
            <audio controls src={audioUrl} className="w-full" />
            {jsonResponse && (
              <div className="mt-3">
                <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">JSON Response</span>
                <pre className="mt-1.5 bg-surface-card border border-hairline-soft rounded-mini-lg px-3 py-2.5 text-xs font-mono text-ink overflow-x-auto whitespace-pre-wrap break-all">
                  {JSON.stringify({ format: jsonResponse.format, audio: jsonResponse.audio ? `${jsonResponse.audio.substring(0, 100)}...` : "" }, null, 2)}
                </pre>
              </div>
            )}
          </div>
        ) : (
          <div>
            <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">Response</span>
            <pre className="mt-1.5 bg-surface-card border border-hairline-soft rounded-mini-lg px-3 py-2.5 text-xs font-mono text-ink overflow-x-auto whitespace-pre-wrap break-all opacity-50">{DEFAULT_TTS_RESPONSE_EXAMPLE}</pre>
          </div>
        )}
      </div>
    </Card>
  );
}
