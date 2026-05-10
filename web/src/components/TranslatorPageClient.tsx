"use client";

import { useState } from "react";
import { Card, Button } from "@/shared/components";

const DEFAULT_REQUEST = JSON.stringify(
  {
    model: "claude-sonnet-4",
    messages: [{ role: "user", content: "Hello" }],
    stream: false,
  },
  null,
  2
);

export default function TranslatorPageClient() {
  const [sourceText, setSourceText] = useState<string>(DEFAULT_REQUEST);
  const [translatedText, setTranslatedText] = useState<string>("");
  const [error, setError] = useState<string>("");
  const [loading, setLoading] = useState<boolean>(false);

  const handleTranslate = async () => {
    if (!sourceText.trim()) return;

    setLoading(true);
    setError("");
    setTranslatedText("");

    let parsedBody: unknown;
    try {
      parsedBody = JSON.parse(sourceText);
    } catch {
      setError("Source must be valid JSON (a request body with a 'model' field).");
      setLoading(false);
      return;
    }

    try {
      // Step 1: detect provider/source/target formats from the request body.
      const detectRes = await fetch("/api/translator/translate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ step: 1, body: parsedBody }),
      });
      const detectJson = await detectRes.json();
      if (!detectRes.ok || detectJson?.success === false) {
        setError(detectJson?.error || `Detect failed (HTTP ${detectRes.status})`);
        return;
      }

      // Step 2: translate the source body into the OpenAI intermediate format.
      const toOpenAiRes = await fetch("/api/translator/translate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ step: 2, body: parsedBody }),
      });
      const toOpenAiJson = await toOpenAiRes.json();
      if (!toOpenAiRes.ok || toOpenAiJson?.success === false) {
        setError(toOpenAiJson?.error || `Translate failed (HTTP ${toOpenAiRes.status})`);
        return;
      }

      setTranslatedText(
        JSON.stringify(
          {
            detected: detectJson.result,
            openaiBody: toOpenAiJson.result?.body,
          },
          null,
          2
        )
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Translator</h1>
      <p className="text-sm text-text-muted">
        Translate a provider-specific request body (Anthropic, Gemini, Cohere, …) into the OpenAI intermediate format used by OpenProxy.
      </p>

      <div className="grid gap-4 lg:grid-cols-2">
        <Card padding="lg">
          <div className="flex flex-col gap-4">
            <label className="text-sm font-medium text-text-muted">Source Request (JSON)</label>
            <textarea
              value={sourceText}
              onChange={(e) => setSourceText(e.target.value)}
              placeholder="Paste a request body to translate..."
              className="w-full h-48 p-3 border border-border rounded-lg bg-bg-subtle resize-none focus:outline-none focus:ring-2 focus:ring-primary/50 font-mono text-sm"
            />
            <Button onClick={handleTranslate} disabled={loading || !sourceText.trim()}>
              {loading ? "Translating..." : "Translate"}
            </Button>
            {error && <p className="text-sm text-red-500">{error}</p>}
          </div>
        </Card>

        <Card padding="lg">
          <div className="flex flex-col gap-4">
            <label className="text-sm font-medium text-text-muted">Translation</label>
            <pre className="w-full h-48 p-3 border border-border rounded-lg bg-bg-subtle overflow-auto font-mono text-sm">
              {translatedText || (
                <span className="text-text-muted">Translation will appear here</span>
              )}
            </pre>
          </div>
        </Card>
      </div>
    </div>
  );
}
