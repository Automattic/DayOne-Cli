import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { test, expect } from "@playwright/test";

/**
 * Live, credentialed end-to-end harness for the journal-create signature work.
 *
 * Goal: prove that an E2E journal created by `dayone journal create --e2e`
 * carries journal-key signatures that the *web client* (and, by extension, the
 * iOS client that shares the same `JournalKeyChecker` verification) accepts —
 * i.e. the new entry's body is readable in the browser after unlocking E2E.
 *
 * This test MUTATES a live account (it creates a journal and an entry) and
 * requires real credentials plus network + an interactive-style web login, so
 * it is SKIPPED BY DEFAULT. It only runs when `E2E_LIVE_JOURNAL_CREATE=1` and
 * the required env vars are set. See e2e/README.md.
 *
 * Web verification reuses the storage-state auth produced by
 * `npx playwright codegen ... --save-storage`, the same mechanism the
 * screenshot suite uses. Unlocking E2E in the browser still needs the master
 * key (`E2E_MASTER_KEY`); if the stored session is not already unlocked, the
 * harness types the key into the unlock prompt.
 */

const apiHost = process.env.E2E_API_HOST ?? "https://stg.dayone.me";
const webBaseUrl = process.env.DAYONE_WEB_BASE_URL ?? apiHost;
const email = process.env.E2E_EMAIL ?? "";
const password = process.env.E2E_PASSWORD ?? "";
// The E2E master key (a.k.a. encryption key, format `D1-<userid>-...`). Used
// both for the CLI (`auth key-set`) and to unlock E2E in the web client.
const masterKey = process.env.E2E_MASTER_KEY ?? process.env.E2E_ENCRYPTION_KEY ?? "";
const storageState = process.env.PLAYWRIGHT_STORAGE_STATE ?? ".auth/staging-user.json";
const liveEnabled = process.env.E2E_LIVE_JOURNAL_CREATE === "1";

const requiredEnv = [
  ["E2E_EMAIL", email],
  ["E2E_PASSWORD", password],
  ["E2E_MASTER_KEY", masterKey]
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

function journalIdFromCreateOutput(raw: string): string {
  const payload = parseJsonOutput(raw) as { journal_id?: unknown; encrypted?: unknown };
  if (typeof payload.journal_id !== "string" || payload.journal_id.length === 0) {
    throw new Error(`journal create output did not include journal_id:\n${raw}`);
  }
  if (payload.encrypted !== true) {
    throw new Error(`journal create did not produce an E2E (encrypted) journal:\n${raw}`);
  }
  return payload.journal_id;
}

// After `sync`, the journal's local `pending-…` id is replaced by the
// server-assigned id. The web client only knows the server id, so resolve it
// by name from `list journals` before navigating.
function resolveSyncedJournalId(configDir: string, name: string): string {
  const raw = runDayone(configDir, ["list", "journals"]);
  const payload = parseJsonOutput(raw) as { items?: Array<Record<string, unknown>> };
  const items = Array.isArray(payload.items) ? payload.items : [];
  for (const item of items) {
    const itemName =
      (item.name as string | undefined) ??
      ((item.data as Record<string, unknown> | undefined)?.name as string | undefined);
    const id = item.id as string | undefined;
    if (itemName === name && typeof id === "string" && id.length > 0 && !id.startsWith("pending-")) {
      return id;
    }
  }
  throw new Error(`could not resolve a synced (non-pending) journal id for "${name}":\n${raw}`);
}

function entryIdFromWriteOutput(raw: string): string {
  const payload = parseJsonOutput(raw) as { entry_id?: unknown };
  if (typeof payload.entry_id !== "string" || payload.entry_id.length === 0) {
    throw new Error(`entry write output did not include entry_id:\n${raw}`);
  }
  return payload.entry_id;
}

function authenticate(configDir: string): void {
  runDayone(configDir, ["auth", "login", "--email", email, "--password-stdin"], {
    input: password
  });
  runDayone(configDir, ["auth", "key-set", "--key-stdin"], { input: masterKey });
}

test.describe("journal create signatures live e2e", () => {
  test.skip(
    !liveEnabled,
    "Set E2E_LIVE_JOURNAL_CREATE=1 to run this live, account-mutating e2e test."
  );
  for (const [name, value] of requiredEnv) {
    test.skip(value.trim().length === 0, `${name} is required for the journal-create signature e2e.`);
  }

  // Day One web stores auth/E2E-unlock state in IndexedDB, which Playwright's
  // storageState does not capture. So don't rely on a saved session — each test
  // authenticates its own context via /test/login (below).
  test.use({ storageState: undefined });

  // Logs into (and E2E-unlocks) the current browser context via the web app's
  // /test/login auto-login, mirroring the use-dayone-web skill.
  async function webLogin(page: import("@playwright/test").Page): Promise<void> {
    const enc = encodeURIComponent;
    const loginUrl = new URL(
      `/test/login?email=${enc(email)}&password=${enc(password)}&key=${enc(masterKey)}&auto=true`,
      webBaseUrl
    ).toString();
    await page.goto(loginUrl, { waitUntil: "domcontentloaded", timeout: 60_000 });
    await page
      .waitForFunction(
        () => !location.href.includes("test/login") && !location.pathname.includes("/login"),
        null,
        { timeout: 60_000 }
      )
      .catch(() => {});
  }

  test("web client reads an entry from a CLI-created E2E journal (signatures accepted)", async ({
    page
  }) => {
    // Heavy live flow (CLI auth/sync/create + web load), slow over a proxy.
    test.setTimeout(300_000);
    const runId = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
    const journalName = `CLI E2E sig check ${runId}`;
    const entryBody = `Signature acceptance probe ${runId}`;
    const configDir = mkdtempSync(path.join(tmpdir(), "dayone-jcreate-sig-"));
    let journalId: string | null = null;

    try {
      // --- CLI: create an E2E journal + entry, then push to the server. -----
      authenticate(configDir);
      runDayone(configDir, ["sync"]);

      const pendingJournalId = journalIdFromCreateOutput(
        runDayone(configDir, ["journal", "create", "--e2e", "--name", journalName])
      );

      const entryId = entryIdFromWriteOutput(
        runDayone(configDir, [
          "entry",
          "write",
          "--journal-id",
          pendingJournalId,
          "--body",
          entryBody
        ])
      );
      runDayone(configDir, ["sync"]);

      // Re-resolve the server-assigned id (the pending id is local-only).
      journalId = resolveSyncedJournalId(configDir, journalName);

      // Authenticate + E2E-unlock this browser context before navigating.
      await webLogin(page);

      // --- Web: open the journal and assert the body is readable. -----------
      // This is the load-bearing assertion: if iOS/web rejected the journal
      // key signatures, the journal key would not be trusted and the entry
      // body could not be decrypted/rendered here.
      // Day One web does lazy per-journal sync: a freshly-pulled journal shows
      // in the sidebar as "not downloaded" until selected. Select it to trigger
      // the download (which decrypts the journal key + entries).
      await page.goto(new URL("/all", webBaseUrl).toString(), {
        waitUntil: "domcontentloaded",
        timeout: 60_000
      });
      const journalButton = page.getByRole("button", { name: new RegExp(runId) });
      await journalButton.click({ timeout: 60_000 });
      await page.waitForLoadState("networkidle", { timeout: 60_000 }).catch(() => {});

      // Now open the entry; the journal (and its key) are downloaded.
      const entryUrl = new URL(`/journals/${journalId}/${entryId}`, webBaseUrl).toString();
      await page.goto(entryUrl, { waitUntil: "domcontentloaded", timeout: 60_000 });
      await page.waitForLoadState("networkidle", { timeout: 30_000 }).catch(() => {});

      // If the session is locked, the web client shows an E2E unlock prompt.
      const unlockField = page.getByPlaceholder(/encryption key|master key/i);
      if (await unlockField.isVisible().catch(() => false)) {
        await unlockField.fill(masterKey);
        await page.getByRole("button", { name: /unlock|continue|submit/i }).click();
        await page.waitForLoadState("networkidle", { timeout: 30_000 }).catch(() => {});
      }

      // The decrypted entry body must render — only possible if the journal key
      // was usable (and, on a signature-verifying build, its signature trusted).
      const body = page.getByText(entryBody, { exact: false }).first();
      try {
        await expect(body).toBeVisible({ timeout: 45_000 });
      } catch {
        await page.reload({ waitUntil: "networkidle", timeout: 60_000 });
        await expect(body).toBeVisible({ timeout: 45_000 });
      }
    } finally {
      // Best-effort cleanup of the created journal (CLI delete if available).
      if (journalId) {
        try {
          runDayone(configDir, ["journal", "delete", "--journal-id", journalId]);
          runDayone(configDir, ["sync"]);
        } catch {
          // Cleanup is best-effort; preserve the original failure.
        }
      }
      rmSync(configDir, { recursive: true, force: true });
    }
  });
});
