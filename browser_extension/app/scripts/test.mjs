import {mkdir} from "node:fs/promises";
import {dirname, resolve} from "node:path";
import {fileURLToPath} from "node:url";
import {spawn} from "node:child_process";
import {build} from "esbuild";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = resolve(rootDir, ".tmp-test");
const testEntry = resolve(rootDir, "tests/download-suffix-filter.test.ts");
const testOutput = resolve(outDir, "download-suffix-filter.test.mjs");

await mkdir(outDir, {recursive: true});
await build({
  entryPoints: [testEntry],
  outfile: testOutput,
  bundle: true,
  format: "esm",
  platform: "node",
  target: "node22",
  external: ["node:test", "node:assert/strict"],
});

const testProcess = spawn(process.execPath, ["--test", testOutput], {
  stdio: "inherit",
});

testProcess.on("exit", (code) => {
  process.exit(code ?? 1);
});
