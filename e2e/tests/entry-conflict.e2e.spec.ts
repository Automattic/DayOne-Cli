import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { test, expect } from "@playwright/test";

const apiHost = process.env.E2E_API_HOST ?? "https://stg.dayone.me";
const journalId = process.env.E2E_JOURNAL_ID ?? "";
const email = process.env.E2E_EMAIL ?? "";
const password = process.env.E2E_PASSWORD ?? "";
const encryptionKey = process.env.E2E_ENCRYPTION_KEY ?? "";
const liveConflictEnabled = process.env.E2E_LIVE_CONFLICT === "1";

const requiredEnv = [
  ["E2E_JOURNAL_ID", journalId],
  ["E2E_EMAIL", email],
  ["E2E_PASSWORD", password]
] as const;

type CliRunOptions = {
  input?: string;
  env?: NodeJS.ProcessEnv;
};

function runDayone(configDir: string, args: string[], options: CliRunOptions = {}): string {
  const result = spawnSync("dayone", ["--api-host", apiHost, ...args], {
    cwd: path.resolve(process.cwd(), ".."),
    encoding: "utf8",
    input: options.input,
    env: {
      ...process.env,
      ...options.env,
      DAYONE_CONFIG_DIR: configDir
    }
  });

  if (result.status !== 0) {
    throw new Error(
      [
        `dayone ${args.join(" ")} failed with status ${result.status}`,
        `stdout:\n${result.stdout}`,
        `stderr:\n${result.stderr}`
      ].join("\n")
    );
  }

  return result.stdout ?? "";
}

function parseJsonOutput(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    const firstBrace = raw.indexOf("{");
    const firstBracket = raw.indexOf("[");
    const starts = [firstBrace, firstBracket].filter((value) => value >= 0);
    if (starts.length === 0) {
      throw new Error(`No JSON payload found in output:\n${raw}`);
    }
    return JSON.parse(raw.slice(Math.min(...starts)));
  }
}

function entryIdFromWriteOutput(raw: string): string {
  const payload = parseJsonOutput(raw) as { entry_id?: unknown };
  if (typeof payload.entry_id !== "string" || payload.entry_id.length === 0) {
    throw new Error(`entry write output did not include entry_id:\n${raw}`);
  }
  return payload.entry_id;
}

function entriesFromListOutput(raw: string): Array<{ id?: unknown; body?: unknown }> {
  const payload = parseJsonOutput(raw);
  if (Array.isArray(payload)) {
    return payload as Array<{ id?: unknown; body?: unknown }>;
  }
  if (payload && typeof payload === "object") {
    const objectPayload = payload as {
      entries?: unknown;
      items?: unknown;
      data?: { entries?: unknown };
    };
    if (Array.isArray(objectPayload.entries)) return objectPayload.entries as Array<{ id?: unknown; body?: unknown }>;
    if (Array.isArray(objectPayload.items)) return objectPayload.items as Array<{ id?: unknown; body?: unknown }>;
    if (objectPayload.data && Array.isArray(objectPayload.data.entries)) {
      return objectPayload.data.entries as Array<{ id?: unknown; body?: unknown }>;
    }
  }
  return [];
}

function latestBodyForEntry(configDir: string, entryId: string): string | null {
  const raw = runDayone(configDir, [
    "list",
    "entries",
    "--journal-id",
    journalId,
    "--limit",
    "1000",
    "--fields",
    "id,body,date"
  ]);
  const match = entriesFromListOutput(raw).find((entry) => entry.id === entryId);
  return typeof match?.body === "string" ? match.body : null;
}

function authenticate(configDir: string): void {
  runDayone(configDir, ["auth", "login", "--email", email, "--password-stdin"], {
    input: password
  });
  if (encryptionKey.trim().length > 0) {
    runDayone(configDir, ["auth", "key-set", "--key-stdin"], { input: encryptionKey });
  }
}

function sync(configDir: string): void {
  runDayone(configDir, ["sync"]);
}

test.describe("entry conflict resolution live e2e", () => {
  test.skip(!liveConflictEnabled, "Set E2E_LIVE_CONFLICT=1 to run this staging-mutating e2e test.");
  for (const [name, value] of requiredEnv) {
    test.skip(value.trim().length === 0, `${name} is required for live conflict e2e.`);
  }

  test("newer local CLI edit wins after another client edits the same staging entry", () => {
    const runId = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
    const configA = mkdtempSync(path.join(tmpdir(), "dayone-conflict-a-"));
    const configB = mkdtempSync(path.join(tmpdir(), "dayone-conflict-b-"));
    const configVerifier = mkdtempSync(path.join(tmpdir(), "dayone-conflict-v-"));
    let entryId: string | null = null;

    const baseBody = `E2E conflict base ${runId}`;
    const remoteBody = `E2E conflict remote edit ${runId}`;
    const cliBody = `E2E conflict CLI winner ${runId}`;

    try {
      authenticate(configA);
      sync(configA);
      entryId = entryIdFromWriteOutput(
        runDayone(configA, ["entry", "write", "--journal-id", journalId, "--body", baseBody])
      );
      sync(configA);

      authenticate(configB);
      sync(configB);
      expect(latestBodyForEntry(configB, entryId)).toContain(baseBody);
      runDayone(configB, [
        "entry",
        "write",
        "--journal-id",
        journalId,
        "--entry-id",
        entryId,
        "--body",
        remoteBody
      ]);
      sync(configB);

      // Client A has not pulled B's edit yet. Make a newer local edit, then sync.
      runDayone(configA, [
        "entry",
        "write",
        "--journal-id",
        journalId,
        "--entry-id",
        entryId,
        "--body",
        cliBody
      ]);
      sync(configA);

      authenticate(configVerifier);
      sync(configVerifier);
      const finalBody = latestBodyForEntry(configVerifier, entryId);
      expect(finalBody).toContain(cliBody);
      expect(finalBody).not.toContain(remoteBody);
    } finally {
      if (entryId) {
        try {
          runDayone(configA, ["entry", "delete", "--journal-id", journalId, "--entry-id", entryId]);
          sync(configA);
        } catch {
          // Best-effort cleanup only; preserve the original test failure if cleanup fails.
        }
      }
      rmSync(configA, { recursive: true, force: true });
      rmSync(configB, { recursive: true, force: true });
      rmSync(configVerifier, { recursive: true, force: true });
    }
  });
});
