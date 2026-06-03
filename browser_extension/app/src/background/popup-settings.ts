export interface BrowserDownloadSuffixFilterSaveResponse {
  ok: true;
  message: string;
  browserDownloadExcludedExtensions: string;
}

export async function saveBrowserDownloadSuffixFilterSetting(
  value: string,
  persist: (nextValue: string) => Promise<void>,
): Promise<BrowserDownloadSuffixFilterSaveResponse> {
  const nextValue = String(value ?? "");
  await persist(nextValue);
  return {
    ok: true,
    message: "后缀排除规则已保存",
    browserDownloadExcludedExtensions: nextValue,
  };
}
