import {describe, it} from "node:test";
import assert from "node:assert/strict";

import {
  inferDownloadExtension,
  normalizeDownloadSuffixes,
} from "../src/background/download-suffix-filter";
import {saveBrowserDownloadSuffixFilterSetting} from "../src/background/popup-settings";
import {
  createRuntimeErrorResponse,
  isPopupStatePayload,
  normalizeRuntimeErrorMessage,
  requirePopupStatePayload,
  runtimeErrorMessageOr,
} from "../src/shared/runtime-messages";

describe("normalizeDownloadSuffixes", () => {
  it("normalizes suffixes with and without dots", () => {
    assert.deepEqual([...normalizeDownloadSuffixes(".zip, exe")], ["zip", "exe"]);
  });

  it("matches suffixes case-insensitively", () => {
    const suffixes = normalizeDownloadSuffixes("ZIP, .Exe");

    assert.equal(suffixes.has("zip"), true);
    assert.equal(suffixes.has("exe"), true);
  });

  it("accepts comma, whitespace, and semicolon separators", () => {
    assert.deepEqual([...normalizeDownloadSuffixes("zip exe; .7z,tar.gz")], [
      "zip",
      "exe",
      "7z",
      "tar.gz",
    ]);
  });

  it("accepts newline-separated suffixes from the popup textarea", () => {
    assert.deepEqual([...normalizeDownloadSuffixes(".zip\nexe\n .7z")], [
      "zip",
      "exe",
      "7z",
    ]);
  });
});

describe("inferDownloadExtension", () => {
  it("ignores URL query strings when inferring the suffix", () => {
    assert.equal(
      inferDownloadExtension({url: "https://example.test/downloads/archive.ZIP?token=file.exe"}),
      "zip",
    );
  });

  it("prefers filename over finalUrl and url", () => {
    assert.equal(
      inferDownloadExtension({
        filename: "/home/user/report.PDF",
        finalUrl: "https://example.test/archive.zip",
        url: "https://example.test/installer.exe",
      }),
      "pdf",
    );
  });

  it("falls back to finalUrl before url", () => {
    assert.equal(
      inferDownloadExtension({
        finalUrl: "https://cdn.example.test/video.MP4?download=1",
        url: "https://origin.example.test/file.zip",
      }),
      "mp4",
    );
  });

  it("returns an empty suffix for downloads without an extension", () => {
    assert.equal(inferDownloadExtension({url: "https://example.test/download"}), "");
  });
});

describe("saveBrowserDownloadSuffixFilterSetting", () => {
  it("responds after storage is saved without waiting for popup state rebuilding", async () => {
    let storedValue = "";
    let rebuildStateCalled = false;
    const neverRebuildsPopupState = () => {
      rebuildStateCalled = true;
      return new Promise<never>(() => {});
    };

    const response = await Promise.race([
      saveBrowserDownloadSuffixFilterSetting(".zip", async (value) => {
        storedValue = value;
      }),
      neverRebuildsPopupState(),
    ]);

    assert.deepEqual(response, {
      ok: true,
      message: "后缀排除规则已保存",
      browserDownloadExcludedExtensions: ".zip",
    });
    assert.equal(storedValue, ".zip");
    assert.equal(rebuildStateCalled, true);
  });
});

describe("normalizeRuntimeErrorMessage", () => {
  it("translates the common message port closed error to Chinese", () => {
    assert.equal(
      normalizeRuntimeErrorMessage("The message port closed before a response was received.", "保存失败"),
      "后台服务没有返回响应，请重新打开扩展后重试",
    );
  });

  it("translates asynchronous message channel closures to Chinese", () => {
    assert.equal(
      normalizeRuntimeErrorMessage(
        "A listener indicated an asynchronous response by returning true, but the message channel closed before a response was received.",
        "保存失败",
      ),
      "后台服务没有返回响应，请重新打开扩展后重试",
    );
  });

  it("translates missing receiving end errors to Chinese", () => {
    assert.equal(
      normalizeRuntimeErrorMessage("Could not establish connection. Receiving end does not exist.", "发送失败"),
      "无法连接到扩展后台，请刷新页面或重新打开扩展后重试",
    );
  });

  it("falls back to the Chinese operation message when no useful message exists", () => {
    assert.equal(normalizeRuntimeErrorMessage("", "保存后缀排除规则失败"), "保存后缀排除规则失败");
  });

  it("does not expose unmapped English errors in popup feedback", () => {
    assert.equal(normalizeRuntimeErrorMessage("Unexpected response shape", "更新后缀排除规则失败"), "更新后缀排除规则失败");
  });
});

describe("runtime error responses", () => {
  it("creates a Chinese error response for rejected background promises", () => {
    assert.deepEqual(createRuntimeErrorResponse(new Error("The message port closed before a response was received."), "操作失败"), {
      ok: false,
      message: "后台服务没有返回响应，请重新打开扩展后重试",
    });
  });

  it("normalizes Error instances before showing popup feedback", () => {
    assert.equal(
      runtimeErrorMessageOr(new Error("Could not establish connection. Receiving end does not exist."), "更新后缀排除规则失败"),
      "无法连接到扩展后台，请刷新页面或重新打开扩展后重试",
    );
  });
});

describe("PopupStatePayload guard", () => {
  const popupState = {
    connectionState: "connected",
    connectionMessage: "已连接",
    desktopVersion: "3.9-1",
    token: "token",
    serverUrl: "http://127.0.0.1:9810",
    interceptDownloads: true,
    browserDownloadExcludedExtensions: ".zip",
    mediaDownloadOverlayEnabled: true,
    tasks: [],
    taskCounters: {total: 0, active: 0, completed: 0},
    resourceState: "ready",
    resourceStateMessage: "资源已就绪",
    currentResources: [],
    otherResources: [],
    tabId: null,
    activePageDomain: "",
    featureStates: {},
    mediaItems: [],
    mediaPlaybackState: {
      available: false,
      message: "",
      tabId: null,
      mediaIndex: -1,
      currentTime: 0,
      duration: 0,
      progress: 0,
      volume: 1,
      paused: true,
      loop: false,
      muted: false,
      speed: 1,
    },
  };

  it("accepts valid popup state payloads", () => {
    assert.equal(isPopupStatePayload(popupState), true);
    assert.equal(requirePopupStatePayload(popupState, "刷新状态失败"), popupState);
  });

  it("rejects error responses instead of treating them as popup state", () => {
    assert.equal(isPopupStatePayload({ok: false, message: "保存失败"}), false);
    assert.throws(
      () => requirePopupStatePayload({ok: false, message: "保存失败"}, "刷新状态失败"),
      /保存失败/,
    );
  });
});
