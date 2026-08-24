import { existsSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const e2eRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(e2eRoot, "..");
const dryRun = process.argv.includes("--dry-run");
const scenarioUrlCachePath = path.join(e2eRoot, "test-results", ".scenario-urls-cache.json");
const scenarioUrlLockDir = path.join(e2eRoot, "test-results", ".scenario-urls-cache.lock");

function requireEnvVars(keys) {
  const missing = keys.filter((key) => {
    const value = process.env[key];
    if (value === undefined) {
      return true;
    }
    const normalized = value.trim();
    return normalized === "" || normalized.includes("<") || normalized.includes(">");
  });
  if (missing.length > 0) {
    throw new Error(
      `Missing required environment variables: ${missing.join(
        ", "
      )}. Set them in e2e/.env (or your shell) before running this flow.`
    );
  }
}

function resolveStorageStatePath() {
  const configured = process.env.PLAYWRIGHT_STORAGE_STATE ?? ".auth/staging-user.json";
  return path.isAbsolute(configured) ? configured : path.join(e2eRoot, configured);
}

function requireStorageState() {
  const storageStatePath = resolveStorageStatePath();
  if (!existsSync(storageStatePath)) {
    throw new Error(
      `Missing Playwright storage state at ${storageStatePath}.\n` +
        "Create it once with:\n" +
        `cd ${e2eRoot} && npx playwright codegen ${process.env.DAYONE_WEB_BASE_URL ?? "https://stg.dayone.me"} --save-storage=${process.env.PLAYWRIGHT_STORAGE_STATE ?? ".auth/staging-user.json"}`
    );
  }
}

function executeCommand(
  command,
  cwd,
  { envOverrides = {}, captureOutput = false, respectDryRun = true } = {}
) {
  console.log(`\n$ ${command}`);
  if (dryRun && respectDryRun) {
    return "";
  }
  const result = spawnSync(command, {
    cwd,
    shell: true,
    stdio: captureOutput ? ["inherit", "pipe", "pipe"] : "inherit",
    env: { ...process.env, ...envOverrides }
  });
  if (captureOutput) {
    if (result.stdout?.length) {
      process.stdout.write(result.stdout);
    }
    if (result.stderr?.length) {
      process.stderr.write(result.stderr);
    }
  }
  if (result.status !== 0) {
    throw new Error(`Command failed with exit code ${result.status}: ${command}`);
  }
  return captureOutput ? result.stdout?.toString("utf8") ?? "" : "";
}

function runCommand(command, cwd, envOverrides = {}) {
  executeCommand(command, cwd, { envOverrides });
}

function readPrepareCommands() {
  const prepareFile =
    process.env.E2E_PREPARE_COMMANDS_FILE ??
    path.join(e2eRoot, "scenarios", "prepare.commands.txt");

  if (process.env.E2E_PREPARE_COMMANDS) {
    return process.env.E2E_PREPARE_COMMANDS.split("\n")
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith("#"));
  }

  if (!existsSync(prepareFile)) {
    console.log(
      "No prepare commands found. Skipping CLI prep step.\n" +
        "Create e2e/scenarios/prepare.commands.txt or set E2E_PREPARE_COMMANDS."
    );
    return [];
  }

  return readFileSync(prepareFile, "utf8")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
}

function parseJsonFromOutput(rawOutput) {
  try {
    return JSON.parse(rawOutput);
  } catch {
    const firstBrace = rawOutput.indexOf("{");
    const firstBracket = rawOutput.indexOf("[");
    const starts = [firstBrace, firstBracket].filter((n) => n >= 0);
    if (starts.length === 0) {
      throw new Error("No JSON payload found in `dayone list journals` output.");
    }
    return JSON.parse(rawOutput.slice(Math.min(...starts)));
  }
}

function extractJournals(payload) {
  if (Array.isArray(payload)) {
    return payload;
  }
  if (!payload || typeof payload !== "object") {
    return [];
  }
  for (const key of ["items", "journals", "data"]) {
    const value = payload[key];
    if (Array.isArray(value)) {
      return value;
    }
    if (value && typeof value === "object" && Array.isArray(value.journals)) {
      return value.journals;
    }
  }
  return [];
}

// The whole point of this suite is to exercise encrypted journals end-to-end
// (now including client-side-encrypted media uploads). Guard against silently
// pointing at a plaintext journal, which would skip the crypto paths entirely.
function verifyTargetJournalIsEncrypted() {
  const journalId = process.env.E2E_JOURNAL_ID;
  const apiHost = process.env.E2E_API_HOST ?? "https://stg.dayone.me";
  const output = executeCommand(
    `dayone --api-host "${apiHost}" list journals --limit 1000`,
    repoRoot,
    { captureOutput: true, respectDryRun: false }
  );

  let payload;
  try {
    payload = parseJsonFromOutput(output);
  } catch (error) {
    throw new Error(
      `Could not parse \`dayone list journals\` output to verify encryption: ${
        error instanceof Error ? error.message : String(error)
      }`
    );
  }

  const journals = extractJournals(payload);
  const target = journals.find((journal) => journal && journal.id === journalId);
  if (!target) {
    throw new Error(
      `Target journal ${journalId} was not found locally after sync, so its ` +
        "encryption status cannot be confirmed. Ensure E2E_JOURNAL_ID points to a " +
        "journal owned by the authenticated account."
    );
  }

  const vault = target.encryption && target.encryption.vault;
  const isEncrypted = vault !== null && typeof vault === "object";
  if (!isEncrypted) {
    throw new Error(
      `Target journal ${journalId} is not end-to-end encrypted. This suite must ` +
        "run against an encrypted journal so the client-side media encryption path " +
        "is exercised. Point E2E_JOURNAL_ID at an encrypted journal (or create one " +
        "with `dayone journal create --e2e`)."
    );
  }

  console.log(`Verified target journal ${journalId} is end-to-end encrypted.`);
}

function clearScenarioUrlCache() {
  if (existsSync(scenarioUrlCachePath)) {
    rmSync(scenarioUrlCachePath, { force: true });
  }
  if (existsSync(scenarioUrlLockDir)) {
    rmSync(scenarioUrlLockDir, { recursive: true, force: true });
  }
}

function main() {
  requireEnvVars([
    "E2E_JOURNAL_ID",
    "E2E_EMAIL",
    "E2E_PASSWORD",
    "E2E_ENCRYPTION_KEY"
  ]);
  requireStorageState();

  const commands = readPrepareCommands();
  if (commands.length > 0) {
    console.log(`Running ${commands.length} prepare command(s) from repo root...`);
    for (const command of commands) {
      runCommand(command, repoRoot);
    }
  }

  if (dryRun) {
    console.log("\nWould verify target journal is end-to-end encrypted...");
  } else {
    verifyTargetJournalIsEncrypted();
  }

  if (dryRun) {
    console.log("\nWould clear screenshot scenario URL cache before capture...");
  } else {
    clearScenarioUrlCache();
    console.log("\nCleared screenshot scenario URL cache before capture.");
  }

  console.log("\nRunning screenshot capture suite...");
  const grep = process.env.E2E_SCREENSHOT_GREP?.trim();
  if (grep) {
    const escaped = grep.replace(/"/g, '\\"');
    runCommand(`npm run screenshots -- --grep "${escaped}"`, e2eRoot);
  } else {
    runCommand("npm run screenshots", e2eRoot);
  }
}

try {
  main();
} catch (error) {
  console.error(
    `\nScreenshot flow failed: ${error instanceof Error ? error.message : String(error)}`
  );
  process.exit(1);
}
