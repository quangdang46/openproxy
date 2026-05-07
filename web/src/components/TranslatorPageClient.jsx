"use client";

import { useState } from "react";
import { Card, Button, Input } from "@/shared/components";

export default function TranslatorPageClient() {
  const [sourceText, setSourceText] = useState("");
  const [translatedText, setTranslatedText] = useState("");
  const [loading, setLoading] = useState(false);

  const handleTranslate = async () => {
    if (!sourceText.trim()) return;

    setLoading(true);
    try {
      const res = await fetch("/api/translate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text: sourceText }),
      });

      if (res.ok) {
        const data = await res.json();
        setTranslatedText(data.translatedText || "");
      }
    } catch (error) {
      console.error("Translation failed:", error);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Translator</h1>

      <div className="grid gap-4 lg:grid-cols-2">
        <Card padding="lg">
          <div className="flex flex-col gap-4">
            <label className="text-sm font-medium text-text-muted">Source Text</label>
            <textarea
              value={sourceText}
              onChange={(e) => setSourceText(e.target.value)}
              placeholder="Enter text to translate..."
              className="w-full h-48 p-3 border border-border rounded-lg bg-bg-subtle resize-none focus:outline-none focus:ring-2 focus:ring-primary/50"
            />
            <Button onClick={handleTranslate} disabled={loading || !sourceText.trim()}>
              {loading ? "Translating..." : "Translate"}
            </Button>
          </div>
        </Card>

        <Card padding="lg">
          <div className="flex flex-col gap-4">
            <label className="text-sm font-medium text-text-muted">Translation</label>
            <div className="w-full h-48 p-3 border border-border rounded-lg bg-bg-subtle overflow-y-auto">
              {translatedText || <span className="text-text-muted">Translation will appear here</span>}
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
}
