import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

test("launcher forwards arguments, installation method, output and exit status", () => {
  const dir = mkdtempSync(join(tmpdir(), "dayone-launcher-"));
  try {
    copyFileSync(new URL("run.js", import.meta.url), join(dir, "run.js"));
    writeFileSync(join(dir, "package.json"), '{"type":"module"}');
    const platform = `${process.platform}-${process.arch}`;
    const packages = platform.startsWith("linux-")
      ? [platform, `${platform}-no-embeddings`]
      : [platform];
    for (const platform of packages) {
      const pkg = join(dir, "node_modules", "@dayone", `cli-${platform}`);
      mkdirSync(pkg, { recursive: true });
      writeFileSync(join(pkg, "package.json"), "{}");
      // Node stands in for the platform binary, so this works without shell scripts.
      // Unix Node installations may load libraries relative to their real location.
      if (process.platform === "win32") {
        copyFileSync(process.execPath, join(pkg, "dayone.exe"));
      } else {
        symlinkSync(process.execPath, join(pkg, "dayone"));
      }
    }
    const result = spawnSync(process.execPath, [join(dir, "run.js"), "-e", `
      console.log(JSON.stringify({ install: process.env.DAYONE_INSTALL_METHOD, args: process.argv.slice(1) }));
      console.error("fixture stderr");
      process.exit(7);
    `, "argument with spaces"], {
      encoding: "utf8",
      env: { ...process.env, DAYONE_INSTALL_METHOD: "standalone" },
    });
    assert.equal(result.status, 7, result.stderr || String(result.error || result.signal));
    assert.deepEqual(JSON.parse(result.stdout), { install: "npm", args: ["argument with spaces"] });
    assert.equal(result.stderr, "fixture stderr\n");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
