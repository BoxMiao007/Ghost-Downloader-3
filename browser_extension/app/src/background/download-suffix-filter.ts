export interface DownloadExtensionCandidate {
  filename?: string;
  finalUrl?: string;
  url?: string;
}

export function normalizeDownloadSuffixes(value: string): Set<string> {
  return new Set(
    value
      .split(/[\s,;]+/)
      .map((item) => item.trim().toLowerCase().replace(/^\.+/, ""))
      .filter(Boolean),
  );
}

export function inferDownloadExtension(downloadItem: DownloadExtensionCandidate): string {
  const candidates = [downloadItem.filename, downloadItem.finalUrl, downloadItem.url];

  for (const candidate of candidates) {
    if (!candidate) {
      continue;
    }

    const source = candidate.includes("//")
      ? (() => {
          try {
            return new URL(candidate).pathname;
          } catch {
            return candidate;
          }
        })()
      : candidate;
    const baseName = source.split(/[\\/]/).pop() ?? "";
    const dotIndex = baseName.lastIndexOf(".");
    if (dotIndex >= 0 && dotIndex < baseName.length - 1) {
      return baseName.slice(dotIndex + 1).toLowerCase();
    }
  }

  return "";
}
