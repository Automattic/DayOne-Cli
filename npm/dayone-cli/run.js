#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);

const PLATFORM_PACKAGES = {
  "darwin-arm64": "@dayone/cli-darwin-arm64",
  "darwin-x64": "@dayone/cli-darwin-x64",
  "win32-x64": "@dayone/cli-win32-x64",
};

// Standard Linux artifacts are built on Ubuntu 24.04 and require glibc 2.39.
const STANDARD_LINUX_GLIBC_MIN_VERSION = "2.39";

const LINUX_PACKAGE_VARIANTS = {
  arm64: {
    default: "@dayone/cli-linux-arm64",
    fallback: "@dayone/cli-linux-arm64-no-embeddings",
  },
  x64: {
    default: "@dayone/cli-linux-x64",
    fallback: "@dayone/cli-linux-x64-no-embeddings",
  },
};

const platformKey = `${process.platform}-${process.arch}`;
const packageCandidates = resolvePackageCandidates();
if (!packageCandidates) {
  console.error(
    `Unsupported platform: ${process.platform} ${process.arch}. ` +
    `Supported: ${[
      ...Object.keys(PLATFORM_PACKAGES),
      "linux-x64",
      "linux-arm64",
    ].join(", ")}`
  );
  process.exit(1);
}

const binaryName = process.platform === "win32" ? "dayone.exe" : "dayone";

let binaryPath = null;
let resolvedPkg = null;
for (const candidate of packageCandidates) {
  try {
    binaryPath = join(
      dirname(require.resolve(`${candidate}/package.json`)),
      binaryName
    );
    resolvedPkg = candidate;
    break;
  } catch {
    // Try next candidate.
  }
}

if (!binaryPath || !resolvedPkg) {
  console.error(
    `Could not find a platform package for ${process.platform}/${process.arch}. ` +
    `Tried: ${packageCandidates.join(", ")}. ` +
    `Try reinstalling: npm i -g @dayone/cli`
  );
  process.exit(1);
}

const args = process.argv.slice(2);

try {
  // The binary owns update discovery for both npm and direct installations.
  execFileSync(binaryPath, args, {
    stdio: "inherit",
    env: { ...process.env, DAYONE_INSTALL_METHOD: "npm" },
  });
} catch (e) {
  if (e.signal) {
    process.kill(process.pid, e.signal);
  } else {
    process.exit(e.status ?? 1);
  }
}

function resolvePackageCandidates() {
  if (process.platform !== "linux") {
    const pkg = PLATFORM_PACKAGES[platformKey];
    return pkg ? [pkg] : null;
  }

  const variants = LINUX_PACKAGE_VARIANTS[process.arch];
  if (!variants) {
    return null;
  }

  const glibcVersion = detectGlibcVersion();
  if (isGlibcAtLeast(glibcVersion, STANDARD_LINUX_GLIBC_MIN_VERSION)) {
    return [variants.default, variants.fallback];
  }
  return [variants.fallback, variants.default];
}

function detectGlibcVersion() {
  // Preferred source: Node process report (no subprocess).
  const reportVersion = process.report?.getReport?.()?.header?.glibcVersionRuntime;
  if (typeof reportVersion === "string" && reportVersion.trim()) {
    return reportVersion.trim();
  }

  try {
    const raw = execFileSync("getconf", ["GNU_LIBC_VERSION"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    const match = raw.match(/^glibc\s+([0-9]+(?:\.[0-9]+){0,2})$/i);
    return match?.[1] ?? null;
  } catch {
    return null;
  }
}

function isGlibcAtLeast(version, minimum) {
  if (!version) {
    return false;
  }
  return compareVersion(version, minimum) >= 0;
}

function compareVersion(a, b) {
  const aParts = a.split(".").map((part) => Number(part) || 0);
  const bParts = b.split(".").map((part) => Number(part) || 0);
  const maxLen = Math.max(aParts.length, bParts.length);
  for (let i = 0; i < maxLen; i += 1) {
    const diff = (aParts[i] ?? 0) - (bParts[i] ?? 0);
    if (diff !== 0) {
      return diff;
    }
  }
  return 0;
}
