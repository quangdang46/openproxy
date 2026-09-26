// Caveman compression levels — the single source for both pickers.
//
// 9router keeps this list in one place (src/app/(dashboard)/dashboard/endpoint/
// endpointConstants.js:19-26) and both the endpoint and token-saver pages read
// it. OpenProxy shipped two copies and they drifted: the endpoint page's copy
// had lost the three 文言文 levels, making them unreachable there.

export interface CavemanLevel {
  id: string;
  label: string;
  desc: string;
  wenyan?: boolean;
}

/** Locales for which the 文言文 levels are offered at all. */
export const WENYAN_LOCALES = ["zh-CN", "zh-TW"];

export const CAVEMAN_LEVELS: CavemanLevel[] = [
  { id: "lite", label: "Lite", desc: "Drop filler, keep grammar" },
  { id: "full", label: "Full", desc: "Drop articles, fragments OK" },
  { id: "ultra", label: "Ultra", desc: "Telegraphic, max compression" },
  { id: "wenyan-lite", label: "文 Lite", desc: "Classical Chinese, light compression", wenyan: true },
  { id: "wenyan", label: "文 Full", desc: "Maximum 文言文, 80-90% reduction", wenyan: true },
  { id: "wenyan-ultra", label: "文 Ultra", desc: "Extreme classical compression", wenyan: true },
];
