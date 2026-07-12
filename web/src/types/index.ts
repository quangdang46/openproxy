// Shared TypeScript types for the OpenProxy Astro dashboard

// Theme types
export type Theme = "light" | "dark" | "system";

// Provider types
export interface Provider {
  id: string;
  alias: string;
  name: string;
  icon: string;
  color: string;
  textIcon?: string;
  website?: string;
  notice?: {
    text?: string;
    signupUrl?: string;
    apiKeyUrl?: string;
  };
  deprecated?: boolean;
  deprecationNotice?: string;
  noAuth?: boolean;
  passthroughModels?: boolean;
  modelsFetcher?: {
    url: string;
    type: string;
  };
  serviceKinds?: ServiceKind[];
  ttsConfig?: TTSConfig;
  sttConfig?: STTConfig;
  embeddingConfig?: EmbeddingConfig;
  thinkingConfig?: ThinkingConfig;
  kindNotice?: Record<string, string>;
  hasProviderSpecificData?: boolean;
  searchViaChat?: SearchViaChatConfig;
  hidden?: boolean;
  hiddenKinds?: ServiceKind[];
  /** Display sort weight (lower first). Mirrors 9router registry priority. */
  priority?: number;
  authType?: "oauth" | "apikey" | "cookie";
  authHint?: string;
  searchConfig?: SearchConfig;
  fetchConfig?: FetchConfig;
  imageConfig?: ImageConfig;
}

export type ServiceKind = "llm" | "tts" | "stt" | "embedding" | "image" | "imageToText" | "webSearch" | "webFetch" | "video" | "music";

export interface TTSConfig {
  baseUrl: string;
  authType: "none" | "apikey";
  authHeader: string;
  format: string;
  models: TTSModel[];
}

export interface TTSModel {
  id: string;
  name: string;
}

export type STTFormat =
  | "openai"
  | "deepgram"
  | "assemblyai"
  | "nvidia-asr"
  | "huggingface-asr"
  | "gemini-stt";

export interface STTConfig {
  baseUrl: string;
  authType: "none" | "apikey";
  authHeader: "bearer" | "token" | "x-api-key" | "key" | "none";
  format: STTFormat;
  models: STTModel[];
  /** True for providers that require an async upload+poll flow (AssemblyAI). */
  async?: boolean;
}

export interface STTModel {
  id: string;
  name: string;
}

export interface EmbeddingConfig {
  baseUrl: string;
  authType: "apikey";
  authHeader: string;
  models: EmbeddingModel[];
}

export interface EmbeddingModel {
  id: string;
  name: string;
  dimensions: number;
}

export interface ThinkingConfig {
  options: string[];
  defaultMode: string;
  defaultBudgetTokens?: number;
}

export interface SearchViaChatConfig {
  defaultModel: string;
  pricingUrl: string;
  freeTier?: string;
}

export interface SearchConfig {
  baseUrl: string;
  method: "GET" | "POST";
  authType: "apikey" | "none";
  authHeader: string;
  costPerQuery: number;
  freeMonthlyQuota: number;
  searchTypes: string[];
  defaultMaxResults: number;
  maxMaxResults: number;
  timeoutMs: number;
  cacheTTLMs: number;
}

export interface FetchConfig {
  baseUrl: string;
  method: "GET" | "POST";
  authType: "apikey" | "none";
  authHeader: string;
  costPerQuery: number;
  freeMonthlyQuota: number;
  formats: string[];
  maxCharacters: number;
  timeoutMs: number;
}

export interface ImageConfig {
  baseUrl: string;
  method: "GET" | "POST";
  authType: "apikey";
  authHeader: string;
  extraHeaders?: Record<string, string>;
}

export interface MediaProviderKind {
  id: ServiceKind;
  label: string;
  icon: string;
  endpoint: {
    method: "GET" | "POST";
    path: string;
  };
}

export interface AuthMethod {
  id: string;
  name: string;
  icon: string;
}

// Model types
export interface Model {
  id: string;
  name: string;
  provider?: string;
  context?: number;
  pricing?: ModelPricing;
}

export interface ModelPricing {
  input?: number;
  output?: number;
  unit?: string;
}

// Store types
export interface ThemeStore {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  initTheme: () => void;
}

export interface UserStore {
  user: User | null;
  setUser: (user: User | null) => void;
  clearUser: () => void;
}

export interface User {
  id: string;
  email?: string;
  name?: string;
  avatar?: string;
}

export interface ProviderStore {
  providers: Provider[];
  connections: Connection[];
  addConnection: (connection: Connection) => void;
  removeConnection: (id: string) => void;
  updateConnection: (id: string, updates: Partial<Connection>) => void;
}

export interface Connection {
  id: string;
  providerId: string;
  apiKey?: string;
  settings?: Record<string, unknown>;
  createdAt: string;
  updatedAt: string;
}

export interface NotificationStore {
  notifications: Notification[];
  addNotification: (notification: Omit<Notification, "id">) => void;
  removeNotification: (id: string) => void;
  clearNotifications: () => void;
}

export interface Notification {
  id: string;
  type: "success" | "error" | "warning" | "info";
  title: string;
  message?: string;
  duration?: number;
}

// API types
export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
}

export interface ApiError {
  message: string;
  code?: string;
  status?: number;
}

// Component prop types
export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /**
   * Visual style. Claude editorial system uses scarce coral CTAs +
   * cream-canvas secondaries; `primary-on-dark` / `secondary-on-dark`
   * are reserved for content sitting on dark navy product surfaces.
   */
  variant?:
    | "primary"
    | "secondary"
    | "outline"
    | "ghost"
    | "danger"
    | "success"
    | "primary-on-dark"
    | "secondary-on-dark";
  size?: "sm" | "md" | "lg";
  icon?: string;
  iconRight?: string;
  disabled?: boolean;
  loading?: boolean;
  fullWidth?: boolean;
  children?: React.ReactNode;
}

export interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  error?: string;
  hint?: string;
  icon?: string;
  iconRight?: string;
  inputClassName?: string;
}

export interface ModalProps {
  isOpen: boolean;
  onClose: () => void;
  title?: string;
  children?: React.ReactNode;
  size?: "sm" | "md" | "lg" | "xl";
}

// Utility types
export type ClassNameValue = string | number | boolean | undefined | null | ClassNameValue[];
