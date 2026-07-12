"use client";

import { useState, useEffect } from "react";
import { Card } from "@/shared/components";
import { AI_PROVIDERS, getProviderAlias } from "@/shared/constants/providers";
import { getModelsByProviderId } from "@/shared/constants/models";
import { TTS_PROVIDER_CONFIG } from "@/shared/constants/ttsProviders";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import { Row, getModelKind } from "./exampleShared";
import React from "react";

const DEFAULT_TTS_RESPONSE_EXAMPLE = `// Audio will appear here after running.

{
  "format": "mp3",
  "audio": "//NExAANaAIIAUAAANNNNNNNN..."
}`;

type VoiceEntry = {
  id: string;
  name: string;
  gender?: string;
  free_users_allowed?: boolean;
};

type LangEntry = {
  code: string;
  name: string;
  voices?: VoiceEntry[];
};

export function TtsExampleCard({ providerId }: { providerId: string }) {
  const providerAlias = getProviderAlias(providerId);
  const config = TTS_PROVIDER_CONFIG[providerId] || TTS_PROVIDER_CONFIG["edge-tts"] || {
    voiceSource: "api-language" as const,
  };
  const ttsConfig = AI_PROVIDERS[providerId]?.ttsConfig;

  const catalogTtsModels = getModelsByProviderId(providerId).filter(
    (m: any) => getModelKind(m) === "tts",
  );
  const configModels = (ttsConfig?.models || []).map((m) => ({ id: m.id, name: m.name || m.id }));
  const ttsModels = catalogTtsModels.length > 0 ? catalogTtsModels : configModels;
  const hasModelSelector =
    !!config.hasModelSelector && (ttsModels.length > 0 || !!config.modelKey);

  const [selectedVoice, setSelectedVoice] = useState(config.defaultVoiceId || "");
  const [selectedVoiceName, setSelectedVoiceName] = useState("");
  const [voiceId, setVoiceId] = useState(config.defaultVoiceId || "");
  const [countryVoices, setCountryVoices] = useState<VoiceEntry[]>([]);
  const [selectedLang, setSelectedLang] = useState("");
  const [selectedModel, setSelectedModel] = useState(
    () => ttsModels[0]?.id || ttsConfig?.models?.[0]?.id || "",
  );

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

  const [modalOpen, setModalOpen] = useState(false);
  const [languages, setLanguages] = useState<LangEntry[]>([]);
  const [modalLoading, setModalLoading] = useState(false);
  const [modalSearch, setModalSearch] = useState("");
  const [modalError, setModalError] = useState("");
  const [byLang, setByLang] = useState<Record<string, LangEntry>>({});

  useEffect(() => {
    setLocalEndpoint(window.location.origin);
    fetch("/api/keys")
      .then((r) => r.json())
      .then((d: any) => {
        setApiKey((d.keys || []).find((k: any) => k.isActive !== false)?.key || "");
      })
      .catch(() => {});
    fetch("/api/tunnel/status")
      .then((r) => r.json())
      .then((d: any) => {
        if (d.publicUrl) setTunnelEndpoint(d.publicUrl);
      })
      .catch(() => {});

    // Seed default voice for api-language providers (MiniMax, ElevenLabs, …).
    if (config.defaultVoiceId) {
      setSelectedVoice(config.defaultVoiceId);
      setVoiceId(config.defaultVoiceId);
      setSelectedVoiceName(config.defaultVoiceId);
    }

    if (config.voiceSource === "hardcoded") {
      const voices = getModelsByProviderId(config.voiceKey || providerId).filter(
        (m: any) => getModelKind(m) === "tts",
      );
      if (voices.length) {
        if (config.hasBrowseButton) {
          const defaultVoice = voices.find((v: any) => v.id === "en") || voices[0];
          setSelectedLang(defaultVoice.id);
          setSelectedVoice(defaultVoice.id);
          setSelectedVoiceName(defaultVoice.name || defaultVoice.id);
          setCountryVoices([{ id: defaultVoice.id, name: defaultVoice.name || defaultVoice.id }]);
        } else {
          setCountryVoices(
            voices.map((v: any) => ({ id: v.id, name: v.name || v.id, gender: v.gender })),
          );
          setSelectedVoice(voices[0].id);
          setSelectedVoiceName(voices[0].name || voices[0].id);
        }
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providerId]);

  const openModal = async () => {
    setModalOpen(true);
    setModalSearch("");
    setModalError("");
    if (languages.length) return;
    setModalLoading(true);
    try {
      if (config.voiceSource === "hardcoded") {
        const voiceKey = config.voiceKey || providerId;
        const voices = getModelsByProviderId(voiceKey).filter(
          (m: any) => getModelKind(m) === "tts",
        );
        const byLangMap: Record<string, LangEntry> = {};
        for (const v of voices) {
          if (!byLangMap[v.id]) {
            byLangMap[v.id] = {
              code: v.id,
              name: v.name || v.id,
              voices: [{ id: v.id, name: v.name || v.id }],
            };
          }
        }
        setByLang(byLangMap);
        setLanguages(
          Object.values(byLangMap).sort((a, b) => a.name.localeCompare(b.name)),
        );
      } else {
        // Provider-specific apiEndpoint (MiniMax, Deepgram, ElevenLabs, Inworld)
        // falls back to the generic edge-tts / local-device list.
        const url = config.apiEndpoint
          ? config.apiEndpoint
          : `/api/media-providers/tts/voices?provider=${
              providerId === "local-device" ? "local-device" : "edge-tts"
            }`;
        const r = await fetch(url);
        const d = await r.json();
        if (d.error) {
          setModalError(typeof d.error === "string" ? d.error : "Failed to load voices");
          return;
        }
        setLanguages(d.languages || []);
        setByLang(d.byLang || {});
      }
    } catch (e: any) {
      setModalError(e.message || "Failed to load voices");
    } finally {
      setModalLoading(false);
    }
  };

  const handlePickLanguage = (lang: LangEntry) => {
    setModalOpen(false);
    setSelectedLang(lang.code);
    const voices: VoiceEntry[] = (byLang[lang.code]?.voices || lang.voices || []).map((v) => ({
      id: v.id,
      name: v.name || v.id,
      gender: v.gender,
      free_users_allowed: v.free_users_allowed,
    }));
    setCountryVoices(voices);
    if (voices.length) {
      setSelectedVoice(voices[0].id);
      setSelectedVoiceName(voices[0].name);
      if (config.hasVoiceIdInput) setVoiceId(voices[0].id);
    }
  };

  const filteredLanguages = modalSearch
    ? languages.filter(
        (c) =>
          c.name?.toLowerCase().includes(modalSearch.toLowerCase()) ||
          c.code?.toLowerCase().includes(modalSearch.toLowerCase()),
      )
    : languages;

  const endpoint = useTunnel ? tunnelEndpoint : localEndpoint;
  const activeVoiceId = config.hasVoiceIdInput ? voiceId || selectedVoice : selectedVoice;
  const modelFull = (() => {
    if (hasModelSelector && selectedModel && activeVoiceId) {
      return `${providerAlias}/${selectedModel}/${activeVoiceId}`;
    }
    if (hasModelSelector && selectedModel) return `${providerAlias}/${selectedModel}`;
    if (activeVoiceId) return `${providerAlias}/${activeVoiceId}`;
    // Fall back to alias alone so the example still runs for config-only providers.
    if (ttsModels[0]?.id) return `${providerAlias}/${ttsModels[0].id}`;
    return providerAlias;
  })();

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
      const res = await fetch(url, {
        method: "POST",
        headers,
        body: JSON.stringify({ ...ttsBody, input: input.trim() }),
      });
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
    <>
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
                    useTunnel
                      ? "border-brand-coral/40 bg-brand-coral/10 text-brand-coral"
                      : "border-hairline text-text-muted hover:text-brand-coral"
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
              {apiKey ? (
                `${apiKey.slice(0, 8)}${"•".repeat(Math.min(20, apiKey.length - 8))}`
              ) : (
                <span className="text-text-muted italic">No key configured</span>
              )}
            </span>
          </Row>

          {hasModelSelector && ttsModels.length > 0 && (
            <Row label="Model">
              <select
                value={selectedModel}
                onChange={(e) => setSelectedModel(e.target.value)}
                className="w-full px-3 py-1.5 text-sm border border-hairline rounded-mini-md bg-canvas focus:outline-none focus:border-brand-coral"
              >
                {ttsModels.map((m: any) => (
                  <option key={m.id} value={m.id}>
                    {m.name || m.id}
                  </option>
                ))}
              </select>
            </Row>
          )}

          {config.hasBrowseButton && (
            <Row label="Language">
              <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
                <button
                  onClick={openModal}
                  className="w-full min-w-0 flex-1 px-3 py-1.5 text-sm border border-hairline rounded-mini-md bg-canvas font-mono truncate text-left hover:border-brand-coral/40 transition-colors"
                >
                  {selectedLang ? (
                    <span className="text-ink">
                      {languages.find((l) => l.code === selectedLang)?.name || selectedLang}
                    </span>
                  ) : (
                    <span className="text-text-muted">No language selected</span>
                  )}
                </button>
                <button
                  onClick={openModal}
                  className="flex w-full items-center justify-center gap-1 text-xs px-2.5 py-1.5 rounded-mini-md border border-hairline text-text-muted hover:text-brand-coral hover:border-brand-coral/40 transition-colors sm:w-auto sm:shrink-0"
                >
                  <span className="material-symbols-outlined text-[14px]">language</span>
                  Select language
                </button>
              </div>
            </Row>
          )}

          {countryVoices.length > 0 && (
            <Row label="Voice">
              <div className="flex flex-wrap gap-1.5">
                {countryVoices.map((v) => (
                  <button
                    key={v.id}
                    onClick={() => {
                      setSelectedVoice(v.id);
                      setSelectedVoiceName(v.name);
                      if (config.hasVoiceIdInput) setVoiceId(v.id);
                    }}
                    className={`px-2.5 py-1 rounded-full text-xs border transition-colors ${
                      selectedVoice === v.id
                        ? "bg-brand-coral/15 border-brand-coral/40 text-brand-coral font-medium"
                        : "border-hairline text-text-muted hover:text-brand-coral hover:border-brand-coral/40"
                    }`}
                  >
                    {v.name}
                    {v.gender ? ` · ${v.gender[0].toUpperCase()}` : ""}
                    {v.free_users_allowed === true && (
                      <span className="ml-1.5 px-1 py-0.5 text-[9px] font-semibold rounded bg-green-500/15 text-green-600 border border-green-500/20">
                        Free
                      </span>
                    )}
                    {v.free_users_allowed === false && (
                      <span className="ml-1.5 px-1 py-0.5 text-[9px] font-semibold rounded bg-amber-500/15 text-amber-600 border border-amber-500/20">
                        Paid
                      </span>
                    )}
                  </button>
                ))}
              </div>
            </Row>
          )}

          {config.hasVoiceIdInput && (
            <Row label="Voice ID">
              <div className="relative">
                <input
                  value={voiceId}
                  onChange={(e) => {
                    setVoiceId(e.target.value);
                    setSelectedVoice(e.target.value);
                  }}
                  placeholder={config.defaultVoiceId || "e.g. English_expressive_narrator"}
                  className="w-full px-3 py-1.5 pr-7 text-sm border border-hairline rounded-mini-md bg-canvas focus:outline-none focus:border-brand-coral font-mono"
                />
                {voiceId && (
                  <button
                    type="button"
                    onClick={() => {
                      setVoiceId("");
                      setSelectedVoice("");
                    }}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-text-muted hover:text-brand-coral transition-colors"
                  >
                    <span className="material-symbols-outlined text-[14px]">close</span>
                  </button>
                )}
              </div>
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
              <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">
                Request
              </span>
              <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
                <button
                  onClick={() => copyCurl(curlSnippet)}
                  className="inline-flex items-center gap-1 text-xs text-text-muted hover:text-brand-coral transition-colors"
                >
                  <span className="material-symbols-outlined text-[14px]">
                    {copiedCurl ? "check" : "content_copy"}
                  </span>
                  {copiedCurl ? "Copied" : "Copy"}
                </button>
                <button
                  onClick={handleRun}
                  disabled={running || !input.trim() || !modelFull}
                  className="flex w-full sm:w-auto items-center justify-center gap-1.5 px-3 py-1 rounded-mini-md bg-brand-coral text-white text-xs font-medium hover:bg-brand-coral/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <span
                    className="material-symbols-outlined text-[14px]"
                    style={running ? { animation: "spin 1s linear infinite" } : undefined}
                  >
                    play_arrow
                  </span>
                  {running ? "Generating..." : "Run"}
                </button>
              </div>
            </div>
            <pre className="bg-surface-card border border-hairline-soft rounded-mini-lg px-3 py-2.5 text-xs font-mono text-ink overflow-x-auto whitespace-pre-wrap break-all">
              {curlSnippet}
            </pre>
          </div>

          {error && <p className="text-xs text-red-500 break-words">{error}</p>}

          {audioUrl ? (
            <div>
              <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between mb-1.5">
                <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">
                  Response{" "}
                  {latency != null && (
                    <span className="font-normal normal-case">({latency}ms)</span>
                  )}
                  {selectedVoiceName ? (
                    <span className="font-normal normal-case text-text-muted">
                      {" "}
                      · {selectedVoiceName}
                    </span>
                  ) : null}
                </span>
                <a
                  href={audioUrl}
                  download="speech.mp3"
                  className="inline-flex items-center gap-1 text-xs text-text-muted hover:text-brand-coral transition-colors"
                >
                  <span className="material-symbols-outlined text-[14px]">download</span>
                  Download
                </a>
              </div>
              <audio controls src={audioUrl} className="w-full" />
              {jsonResponse && (
                <div className="mt-3">
                  <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">
                    JSON Response
                  </span>
                  <pre className="mt-1.5 bg-surface-card border border-hairline-soft rounded-mini-lg px-3 py-2.5 text-xs font-mono text-ink overflow-x-auto whitespace-pre-wrap break-all">
                    {JSON.stringify(
                      {
                        format: jsonResponse.format,
                        audio: jsonResponse.audio
                          ? `${jsonResponse.audio.substring(0, 100)}...`
                          : "",
                      },
                      null,
                      2,
                    )}
                  </pre>
                </div>
              )}
            </div>
          ) : (
            <div>
              <span className="text-xs font-semibold text-text-muted uppercase tracking-wider">
                Response
              </span>
              <pre className="mt-1.5 bg-surface-card border border-hairline-soft rounded-mini-lg px-3 py-2.5 text-xs font-mono text-ink overflow-x-auto whitespace-pre-wrap break-all opacity-50">
                {DEFAULT_TTS_RESPONSE_EXAMPLE}
              </pre>
            </div>
          )}
        </div>
      </Card>

      {modalOpen && (
        <div
          className="fixed inset-0 z-50 flex items-end justify-center sm:items-center"
          style={{ backgroundColor: "rgba(0,0,0,0.6)", backdropFilter: "blur(2px)" }}
          onClick={() => setModalOpen(false)}
        >
          <div
            className="border border-hairline rounded-mini-lg shadow-2xl w-full max-w-md mx-4 flex flex-col max-h-[80vh] bg-canvas"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between px-4 py-3 border-b border-hairline shrink-0 rounded-t-mini-lg">
              <h3 className="text-sm font-semibold">Select Language</h3>
              <button
                onClick={() => setModalOpen(false)}
                className="text-text-muted hover:text-brand-coral transition-colors"
              >
                <span className="material-symbols-outlined text-[20px]">close</span>
              </button>
            </div>

            <div className="px-4 py-2.5 border-b border-hairline shrink-0">
              <input
                autoFocus
                value={modalSearch}
                onChange={(e) => setModalSearch(e.target.value)}
                placeholder="Search language..."
                className="w-full px-3 py-1.5 text-sm border border-hairline rounded-mini-md bg-canvas focus:outline-none focus:border-brand-coral"
              />
            </div>

            <div className="overflow-y-auto flex-1 p-2">
              {modalError && <p className="text-xs text-red-500 px-2 py-1">{modalError}</p>}
              {modalLoading ? (
                <p className="text-xs text-text-muted px-2 py-3">Loading...</p>
              ) : (
                <div className="flex flex-col gap-0.5">
                  {filteredLanguages.map((c) => {
                    const voiceCount =
                      c.voices?.length ?? byLang[c.code]?.voices?.length ?? 0;
                    return (
                      <button
                        key={c.code}
                        onClick={() => handlePickLanguage(c)}
                        className={`flex items-center justify-between w-full px-3 py-2 rounded-mini-md text-left hover:bg-surface-card transition-colors ${
                          selectedLang === c.code ? "bg-brand-coral/10 text-brand-coral" : ""
                        }`}
                      >
                        <span className="text-sm">{c.name || c.code}</span>
                        <div className="flex items-center gap-2 shrink-0">
                          <span className="text-xs text-text-muted">{voiceCount} voices</span>
                          {selectedLang === c.code && (
                            <span className="material-symbols-outlined text-[16px] text-brand-coral">
                              check
                            </span>
                          )}
                        </div>
                      </button>
                    );
                  })}
                  {filteredLanguages.length === 0 && (
                    <p className="text-xs text-text-muted px-2 py-3">No languages found.</p>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </>
  );
}
