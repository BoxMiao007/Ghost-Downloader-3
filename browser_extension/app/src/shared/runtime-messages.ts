import type {PopupStatePayload} from "./types";

export interface RuntimeErrorResponse {
  ok: false;
  message: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" ? value as Record<string, unknown> : null;
}

export function normalizeRuntimeErrorMessage(message: unknown, fallback: string): string {
  const text = String(message ?? "").trim();
  if (!text) {
    return fallback;
  }

  if (
    /message port closed before a response/i.test(text)
    || /message channel closed before a response/i.test(text)
  ) {
    return "后台服务没有返回响应，请重新打开扩展后重试";
  }

  if (/receiving end does not exist/i.test(text)) {
    return "无法连接到扩展后台，请刷新页面或重新打开扩展后重试";
  }

  if (/^[\x00-\x7F]+$/.test(text) && /[a-z]/i.test(text)) {
    return fallback;
  }

  return text;
}

export function runtimeErrorMessageOr(error: unknown, fallback: string): string {
  if (error instanceof Error) {
    return normalizeRuntimeErrorMessage(error.message, fallback);
  }
  return normalizeRuntimeErrorMessage(error, fallback);
}

export function createRuntimeErrorResponse(error: unknown, fallback: string): RuntimeErrorResponse {
  return {
    ok: false,
    message: runtimeErrorMessageOr(error, fallback),
  };
}

export function isRuntimeErrorResponse(value: unknown): value is RuntimeErrorResponse {
  const record = asRecord(value);
  return record?.ok === false && typeof record.message === "string";
}

export function isPopupStatePayload(value: unknown): value is PopupStatePayload {
  const record = asRecord(value);
  return Boolean(
    record
    && typeof record.connectionState === "string"
    && typeof record.connectionMessage === "string"
    && typeof record.desktopVersion === "string"
    && typeof record.token === "string"
    && typeof record.serverUrl === "string"
    && typeof record.interceptDownloads === "boolean"
    && typeof record.browserDownloadExcludedExtensions === "string"
    && typeof record.mediaDownloadOverlayEnabled === "boolean"
    && Array.isArray(record.tasks)
    && asRecord(record.taskCounters)
    && typeof record.resourceState === "string"
    && typeof record.resourceStateMessage === "string"
    && Array.isArray(record.currentResources)
    && Array.isArray(record.otherResources)
    && (record.tabId === null || typeof record.tabId === "number")
    && typeof record.activePageDomain === "string"
    && asRecord(record.featureStates)
    && Array.isArray(record.mediaItems)
    && asRecord(record.mediaPlaybackState)
  );
}

export function requirePopupStatePayload(value: unknown, fallback: string): PopupStatePayload {
  if (isRuntimeErrorResponse(value)) {
    throw new Error(normalizeRuntimeErrorMessage(value.message, fallback));
  }
  if (!isPopupStatePayload(value)) {
    throw new Error(fallback);
  }
  return value;
}
