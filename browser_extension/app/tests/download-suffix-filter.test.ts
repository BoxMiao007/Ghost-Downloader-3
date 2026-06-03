import {describe, it} from "node:test";
import assert from "node:assert/strict";

import {
  inferDownloadExtension,
  normalizeDownloadSuffixes,
} from "../src/background/download-suffix-filter";

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
