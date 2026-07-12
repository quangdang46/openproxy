// Default remote plugins for Claude Cowork (3p managedMcpServers, HTTPS only).

export interface CoworkPlugin {
  name: string;
  title: string;
  description: string;
  url: string;
  transport: "http" | "sse";
  oauth?: boolean;
  toolNames?: string[];
  custom?: boolean;
}

export interface LocalStdioPlugin {
  name: string;
  title: string;
  description: string;
  extensionUrl?: string;
  command: string;
  args: string[];
  toolNames?: string[];
}

export interface ManagedMcpServer {
  name: string;
  url: string;
  transport: string;
  oauth?: boolean;
  toolPolicy?: Record<string, string>;
  custom?: boolean;
  headers?: Record<string, string>;
}

export const DEFAULT_PLUGINS: CoworkPlugin[] = [
  {
    name: "exa",
    title: "Exa",
    description: "Real-time web search and code documentation",
    url: "https://mcp.exa.ai/mcp",
    transport: "http",
    oauth: false,
    toolNames: ["web_search_exa", "web_fetch_exa"],
  },
  {
    name: "tavily",
    title: "Tavily",
    description: "Real-time web search optimized for LLM agents",
    url: "https://mcp.tavily.com/mcp",
    transport: "http",
    oauth: true,
    toolNames: ["tavily_search", "tavily_extract", "tavily_crawl", "tavily_map"],
  },
];

// Local stdio plugins bridged via inline SSE endpoint on the app's port.
export const LOCAL_STDIO_PLUGINS: LocalStdioPlugin[] = [
  {
    name: "browsermcp",
    title: "Browser MCP",
    description: "Control your running Chrome (requires Chrome extension)",
    extensionUrl:
      "https://chromewebstore.google.com/detail/browser-mcp-automate-your/bjfgambnhccakkhmkepdoekmckoijdlc",
    command: "npx",
    args: ["-y", "@browsermcp/mcp@latest"],
    toolNames: [
      "browser_navigate",
      "browser_snapshot",
      "browser_click",
      "browser_type",
      "browser_screenshot",
      "browser_get_console_logs",
      "browser_wait",
      "browser_press_key",
      "browser_go_back",
      "browser_go_forward",
    ],
  },
];

export function buildManagedMcpServers(
  plugins: Array<Partial<CoworkPlugin> | null | undefined> | null | undefined,
): ManagedMcpServer[] {
  const list = Array.isArray(plugins) ? plugins : [];
  const out: ManagedMcpServer[] = [];
  const seen = new Set<string>();

  for (const p of list) {
    if (!p?.name || !p?.url || seen.has(p.name)) continue;
    seen.add(p.name);

    const entry: ManagedMcpServer = {
      name: p.name,
      url: p.url,
      transport: p.transport || (/\/sse(\b|\/)/i.test(p.url) ? "sse" : "http"),
    };

    if (p.oauth) entry.oauth = true;

    if (Array.isArray(p.toolNames) && p.toolNames.length > 0) {
      // Strip any pre-existing "{name}-" prefixes (idempotent across re-applies),
      // then emit both bare + single-prefixed variants to match runtime tool naming.
      const prefix = `${p.name}-`;
      const bare = new Set<string>();
      for (const raw of p.toolNames) {
        if (typeof raw !== "string" || !raw) continue;
        let t = raw;
        while (t.startsWith(prefix)) t = t.slice(prefix.length);
        bare.add(t);
      }
      const policy: Record<string, string> = {};
      for (const t of bare) {
        policy[t] = "allow";
        policy[`${prefix}${t}`] = "allow";
      }
      entry.toolPolicy = policy;
    }

    out.push(entry);
  }

  return out;
}
