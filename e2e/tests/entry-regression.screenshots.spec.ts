import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { test, expect, type Locator, type Page } from "@playwright/test";

const screenshotOutputDir = path.resolve(process.cwd(), "artifacts/screenshots");
const storageStatePath = path.resolve(
  process.cwd(),
  process.env.PLAYWRIGHT_STORAGE_STATE ?? ".auth/staging-user.json"
);
const repoRoot = path.resolve(process.cwd(), "..");
const apiHost = process.env.E2E_API_HOST ?? "https://stg.dayone.me";
const webBaseUrl = process.env.DAYONE_WEB_BASE_URL ?? "https://stg.dayone.me";
const scenarioUrlCachePath = path.resolve(process.cwd(), "test-results/.scenario-urls-cache.json");
const scenarioUrlLockDir = path.resolve(process.cwd(), "test-results/.scenario-urls-cache.lock");
const sleepSignal = new Int32Array(new SharedArrayBuffer(4));

const scenarios = [
  {
    id: "01-plain-text-entry",
    description: "Plain text entry view",
    bodyNeedle: "E2E plain text entry",
    expectedText: "E2E plain text entry"
  },
  {
    id: "02-formatted-markdown-entry",
    description: "Formatted markdown entry view",
    bodyNeedle: "# E2E Formatted Entry",
    expectedText: "E2E Formatted Entry"
  },
  {
    id: "03-plain-text-image-end-entry",
    description: "Plain text with image at end entry view",
    bodyNeedle: "E2E plain text + image at end",
    expectedText: "E2E plain text + image at end"
  },
  {
    id: "04-markdown-image-end-entry",
    description: "Markdown with image at end entry view",
    bodyNeedle: "# E2E Markdown + image",
    expectedText: "E2E Markdown + image"
  },
  {
    id: "05-inline-media-entry",
    description: "Inline media rendering entry view",
    bodyNeedle: "# E2E Inline placeholder",
    expectedText: "E2E Inline placeholder"
  },
  {
    id: "06-video-media-entry",
    description: "Inline video rendering entry view",
    bodyNeedle: "# E2E Video placeholder",
    expectedText: "E2E Video placeholder"
  },
  {
    id: "07-pdf-media-entry",
    description: "Inline PDF rendering entry view",
    bodyNeedle: "# E2E PDF placeholder",
    expectedText: "E2E PDF placeholder"
  }
] as const;

type DayOneEntry = { id?: unknown; body?: unknown; date?: unknown };

let discoveredUrls: Record<string, string> | null = null;

function readCreationDate(entry: DayOneEntry): number {
  const value = entry.date;
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string") {
    const numeric = Number(value);
    if (Number.isFinite(numeric) && value.trim() !== "") {
      return numeric;
    }
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) {
      return parsed;
    }
  }
  return 0;
}

function parseJsonFromOutput(rawOutput: string): unknown {
  try {
    return JSON.parse(rawOutput);
  } catch {
    const firstBrace = rawOutput.indexOf("{");
    const firstBracket = rawOutput.indexOf("[");
    const starts = [firstBrace, firstBracket].filter((n) => n >= 0);
    if (starts.length === 0) {
      throw new Error("No JSON payload found in `dayone list entries` output.");
    }
    return JSON.parse(rawOutput.slice(Math.min(...starts)));
  }
}

function extractEntries(payload: unknown): DayOneEntry[] {
  if (Array.isArray(payload)) {
    return payload;
  }
  if (!payload || typeof payload !== "object") {
    return [];
  }
  if (Array.isArray((payload as { entries?: unknown }).entries)) {
    return (payload as { entries: DayOneEntry[] }).entries;
  }
  const data = (payload as { data?: { entries?: unknown } }).data;
  if (data && Array.isArray(data.entries)) {
    return data.entries as DayOneEntry[];
  }
  if (Array.isArray((payload as { items?: unknown }).items)) {
    return (payload as { items: DayOneEntry[] }).items;
  }
  return [];
}

function findLatestEntryId(entries: DayOneEntry[], bodyNeedle: string): string | null {
  const matches = entries.filter((entry) => {
    const body = typeof entry.body === "string" ? entry.body : "";
    return body.includes(bodyNeedle);
  });
  matches.sort((a, b) => readCreationDate(b) - readCreationDate(a));
  const id = matches[0]?.id;
  return typeof id === "string" && id.length > 0 ? id : null;
}

function normalizeWebBaseUrl(url: string): string {
  return url.replace(/\/$/, "");
}

function sleepSync(ms: number): void {
  Atomics.wait(sleepSignal, 0, 0, ms);
}

function isDatabaseLockedError(message: string): boolean {
  return /database is locked|Error code 5/i.test(message);
}

function runListEntriesWithRetry(journalId: string, maxAttempts = 6): string {
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    const listResult = spawnSync(
      `dayone --api-host "${apiHost}" list entries --journal-id "${journalId}" --limit 1000 --fields id,body,date`,
      {
        cwd: repoRoot,
        shell: true,
        encoding: "utf8",
        env: process.env
      }
    );
    if (listResult.status === 0) {
      return listResult.stdout ?? "";
    }

    const details = listResult.stderr?.trim() || listResult.stdout?.trim() || "unknown error";
    if (!isDatabaseLockedError(details) || attempt === maxAttempts) {
      throw new Error(`Failed to resolve screenshot URLs from dayone list entries: ${details}`);
    }

    sleepSync(attempt * 300);
  }

  throw new Error("Failed to resolve screenshot URLs from dayone list entries.");
}

function readScenarioUrlCache(): Record<string, string> | null {
  if (!existsSync(scenarioUrlCachePath)) {
    return null;
  }

  try {
    const parsed = JSON.parse(readFileSync(scenarioUrlCachePath, "utf8")) as unknown;
    if (!parsed || typeof parsed !== "object") {
      return null;
    }

    const cached = parsed as Record<string, unknown>;
    const urls: Record<string, string> = {};
    for (const scenario of scenarios) {
      const value = cached[scenario.id];
      if (typeof value !== "string" || value.length === 0) {
        return null;
      }
      urls[scenario.id] = value;
    }
    return urls;
  } catch {
    return null;
  }
}

function withScenarioUrlLock(computeUrls: () => Record<string, string>): Record<string, string> {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    try {
      mkdirSync(scenarioUrlLockDir);
      try {
        const fromCache = readScenarioUrlCache();
        if (fromCache) {
          return fromCache;
        }

        const computed = computeUrls();
        mkdirSync(path.dirname(scenarioUrlCachePath), { recursive: true });
        writeFileSync(scenarioUrlCachePath, JSON.stringify(computed), "utf8");
        return computed;
      } finally {
        rmSync(scenarioUrlLockDir, { recursive: true, force: true });
      }
    } catch (error) {
      if (!(error instanceof Error) || !error.message.includes("EEXIST")) {
        throw error;
      }

      const fromCache = readScenarioUrlCache();
      if (fromCache) {
        return fromCache;
      }
      sleepSync(250);
    }
  }

  throw new Error("Timed out waiting for screenshot URL discovery lock.");
}

function resolveScenarioUrls(): Record<string, string> {
  if (discoveredUrls) {
    return discoveredUrls;
  }

  const fromCache = readScenarioUrlCache();
  if (fromCache) {
    discoveredUrls = fromCache;
    return discoveredUrls;
  }

  const journalId = process.env.E2E_JOURNAL_ID;
  if (!journalId) {
    throw new Error(
      "Missing E2E_JOURNAL_ID. Set it to enable automatic screenshot URL discovery."
    );
  }

  discoveredUrls = withScenarioUrlLock(() => {
    const output = runListEntriesWithRetry(journalId);
    const entries = extractEntries(parseJsonFromOutput(output));
    if (entries.length === 0) {
      throw new Error("Could not discover entries for screenshot URL resolution.");
    }

    const base = normalizeWebBaseUrl(webBaseUrl);
    const urls: Record<string, string> = {};
    for (const scenario of scenarios) {
      const entryId = findLatestEntryId(entries, scenario.bodyNeedle);
      if (!entryId) {
        throw new Error(
          `Could not find latest entry for scenario "${scenario.id}" using marker "${scenario.bodyNeedle}".`
        );
      }
      urls[scenario.id] = `${base}/journals/${journalId}/${entryId}`;
    }
    return urls;
  });

  return discoveredUrls;
}

function resolveScenarioUrl(scenarioId: string): string {
  const resolved = resolveScenarioUrls()[scenarioId];
  if (!resolved) {
    throw new Error(`No URL discovered for screenshot scenario "${scenarioId}".`);
  }
  return resolved;
}

async function preparePageForStableCapture(page: Page): Promise<void> {
  await page.addStyleTag({
    content: `
      *, *::before, *::after {
        animation: none !important;
        transition: none !important;
        caret-color: transparent !important;
      }
    `
  });
}

async function captureScenario(page: Page, filename: string): Promise<void> {
  mkdirSync(screenshotOutputDir, { recursive: true });
  const screenshotPath = path.join(screenshotOutputDir, `${filename}.png`);
  await page.screenshot({ path: screenshotPath, fullPage: true });
}

function isLoginUrl(url: string): boolean {
  return /(\/login|\/signin)/i.test(url);
}

function parseJournalEntryUrl(targetUrl: string): {
  journalId: string;
  entryId: string;
  origin: string;
  journalUrl: string;
} | null {
  try {
    const parsed = new URL(targetUrl);
    const match = parsed.pathname.match(/^\/journals\/([^/]+)\/([^/?#]+)/);
    if (!match) {
      return null;
    }
    const journalId = match[1];
    const entryId = match[2];
    return {
      journalId,
      entryId,
      origin: parsed.origin,
      journalUrl: `${parsed.origin}/journals/${journalId}`
    };
  } catch {
    return null;
  }
}

function extractEntryIdFromUrl(targetUrl: string): string | null {
  const parsed = parseJournalEntryUrl(targetUrl);
  if (parsed) {
    return parsed.entryId;
  }
  const fallback = targetUrl.match(/\/journals\/[^/]+\/([^/?#]+)/);
  return fallback?.[1] ?? null;
}

function isLikelyOnTargetEntry(currentUrl: string, targetUrl: string): boolean {
  const targetEntryId = extractEntryIdFromUrl(targetUrl);
  if (!targetEntryId) {
    return false;
  }
  return currentUrl.includes(targetEntryId);
}

function isOnTargetEntry(currentUrl: string, targetUrl: string): boolean {
  const current = parseJournalEntryUrl(currentUrl);
  const target = parseJournalEntryUrl(targetUrl);
  if (!current || !target) {
    return isLikelyOnTargetEntry(currentUrl, targetUrl);
  }
  return current.journalId === target.journalId && current.entryId === target.entryId;
}

async function clickIfVisible(selector: Locator) {
  if ((await selector.count()) > 0 && (await selector.first().isVisible())) {
    try {
      await selector.first().click({ force: true, timeout: 2000 });
      return true;
    } catch {
      return false;
    }
  }
  return false;
}

async function dismissBlockingModal(page: Page): Promise<void> {
  await page.keyboard.press("Escape").catch(() => {});
  await clickIfVisible(page.getByRole("button", { name: /^Close$/i }));
  await clickIfVisible(page.getByRole("button", { name: /Hide this banner/i }));
}

async function submitLoginForm(page: Page, email: string, password: string): Promise<void> {
  let emailInput = page.getByLabel("Email address");
  if ((await emailInput.count()) === 0) {
    emailInput = page.locator('input[type="email"]');
  }
  let passwordInput = page.getByLabel("Password");
  if ((await passwordInput.count()) === 0) {
    passwordInput = page.locator('input[type="password"]');
  }

  await emailInput.first().fill(email);
  await passwordInput.first().fill(password);
  await passwordInput.first().press("Enter").catch(() => {});
  if (isLoginUrl(page.url())) {
    const loginButton = page.getByRole("button", { name: /log in/i });
    if ((await loginButton.count()) > 0) {
      await loginButton.first().click();
    }
  }
}

async function refreshSessionIfNeeded(page: Page, targetUrl: string): Promise<void> {
  const email = process.env.E2E_EMAIL;
  const password = process.env.E2E_PASSWORD;
  if (!email || !password) {
    throw new Error(
      "Redirected to login and missing E2E_EMAIL/E2E_PASSWORD for automatic re-login."
    );
  }

  await submitLoginForm(page, email, password);
  await page
    .waitForURL((url) => !isLoginUrl(url.toString()), { timeout: 30_000 })
    .catch(() => {});

  if (isLoginUrl(page.url())) {
    throw new Error(
      "Auto-login did not complete. Credentials may be invalid, additional verification may be required, or login selectors changed."
    );
  }

  mkdirSync(path.dirname(storageStatePath), { recursive: true });
  await page.context().storageState({ path: storageStatePath });

  await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
  await page.waitForLoadState("load", { timeout: 10_000 }).catch(() => {});
}

async function ensureAuthenticated(page: Page, targetUrl: string): Promise<void> {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    if (!isLoginUrl(page.url())) {
      await page.waitForTimeout(1500);
    }
    if (!isLoginUrl(page.url())) {
      return;
    }
    await refreshSessionIfNeeded(page, targetUrl);
  }
}

async function waitForLoadingToSettle(page: Page, maxWaitMs = 30_000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    const loadingCount = await page.getByText("Loading...").count();
    if (loadingCount === 0) {
      return;
    }
    await page.waitForTimeout(1000);
  }
}

async function unlockJournalsIfNeeded(page: Page): Promise<void> {
  const key = process.env.E2E_ENCRYPTION_KEY;
  if (!key) {
    return;
  }

  const lockedPageMessage = page.getByText(/Journal with ID .* is still locked/i);
  const unlockWithKeyButton = page.getByRole("button", {
    name: /Click to unlock journals with key/i
  });
  const lockedHiddenButton = page.getByRole("button", {
    name: /Locked Journals Hidden/i
  });
  if (
    (await lockedPageMessage.count()) === 0 &&
    (await unlockWithKeyButton.count()) === 0 &&
    (await lockedHiddenButton.count()) === 0
  ) {
    return;
  }

  await clickIfVisible(unlockWithKeyButton);
  await clickIfVisible(lockedHiddenButton);

  const keyInput = page.locator(
    "#encryption_key, input[placeholder='D1-...'], input[name='encryption_key'], input[type='password']"
  );
  // The web app opens the encryption-key entry in an async modal, so the input
  // is not present in the DOM on the same tick the unlock button is clicked.
  // Wait for it to render before filling, otherwise the key is never entered
  // and the encrypted journal stays locked/undownloaded.
  await keyInput
    .first()
    .waitFor({ state: "visible", timeout: 10_000 })
    .catch(() => {});
  if ((await keyInput.count()) > 0) {
    await keyInput.first().fill(key);
    await keyInput.first().press("Enter").catch(() => {});
    const saveButton = page.getByRole("button", { name: /save|add/i });
    if ((await saveButton.count()) > 0) {
      await saveButton.first().click().catch(() => {});
    }
    // Adding the key kicks off a download of the now-unlocked journal. Wait for
    // the success confirmation (best effort) before continuing.
    await page
      .getByText(/encryption key was successfully added/i)
      .first()
      .waitFor({ state: "visible", timeout: 15_000 })
      .catch(() => {});
    await page.waitForTimeout(2000);
  }
}

async function openEntryViaTimeline(
  page: Page,
  targetUrl: string
): Promise<boolean> {
  const route = parseJournalEntryUrl(targetUrl);
  if (!route) {
    return false;
  }

  const candidateSources = [`${route.origin}/all`, route.journalUrl];
  for (const sourceUrl of candidateSources) {
    await page.goto(sourceUrl, { waitUntil: "domcontentloaded" });
    await page.waitForLoadState("load", { timeout: 10_000 }).catch(() => {});
    await ensureAuthenticated(page, sourceUrl);
    await waitForLoadingToSettle(page, 30_000);

    for (let attempt = 0; attempt < 3; attempt += 1) {
      await dismissBlockingModal(page);
      const entryById = page.locator(`a[href$="/${route.entryId}"]`);
      if ((await entryById.count()) > 0) {
        await entryById.first().click({ force: true });
        await page.waitForLoadState("load", { timeout: 10_000 }).catch(() => {});
        if (isLikelyOnTargetEntry(page.url(), targetUrl)) {
          return true;
        }
      }

      await clickIfVisible(page.getByRole("button", { name: /sync is idle.*sync now/i }));
      await unlockJournalsIfNeeded(page);
      await page.waitForTimeout(2000);
    }
  }

  return false;
}

async function waitForScenarioReady(
  page: Page,
  targetUrl: string,
  expectedText: string
): Promise<void> {
  for (let attempt = 0; attempt < 5; attempt += 1) {
    await ensureAuthenticated(page, targetUrl);
    await unlockJournalsIfNeeded(page);
    await waitForLoadingToSettle(page, 10_000);

    const journalNotFound = page.getByText(/Journal with ID .* not found/i);
    if ((await journalNotFound.count()) > 0) {
      const opened = await openEntryViaTimeline(page, targetUrl);
      if (opened) {
        return;
      }
    }

    const mainPanel = page.getByLabel("Main Panel");
    const readyTarget = mainPanel.getByText(expectedText, { exact: false });
    const fallbackReadyTarget = page.getByText(expectedText, { exact: false });
    if (
      isOnTargetEntry(page.url(), targetUrl) &&
      (((await readyTarget.count()) > 0 &&
        (await readyTarget.first().isVisible())) ||
        ((await fallbackReadyTarget.count()) > 0 &&
          (await fallbackReadyTarget.first().isVisible())))
    ) {
      return;
    }

    const loading = page.getByText("Loading...");
    const noEntries = page.getByText("No Entries");
    const isStillLoading = (await loading.count()) > 0 || (await noEntries.count()) > 0;

    if (isStillLoading) {
      await clickIfVisible(page.getByRole("button", { name: /sync is idle.*sync now/i }));
      await unlockJournalsIfNeeded(page);
      await waitForLoadingToSettle(page, 10_000);
      const opened = await openEntryViaTimeline(page, targetUrl);
      if (opened) {
        return;
      }
      continue;
    }

    await page.waitForTimeout(1500);
  }

  // Final recovery pass for parallel runs where background sync lags behind.
  await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
  await page.waitForLoadState("load", { timeout: 10_000 }).catch(() => {});
  await ensureAuthenticated(page, targetUrl);
  await unlockJournalsIfNeeded(page);
  await waitForLoadingToSettle(page, 10_000);

  const recovered = await openEntryViaTimeline(page, targetUrl);
  if (recovered) {
    return;
  }

  const recoveredTarget = page
    .getByLabel("Main Panel")
    .getByText(expectedText, { exact: false });
  const recoveredFallbackTarget = page.getByText(expectedText, { exact: false });
  if (
    isOnTargetEntry(page.url(), targetUrl) &&
    (((await recoveredTarget.count()) > 0 &&
      (await recoveredTarget.first().isVisible())) ||
      ((await recoveredFallbackTarget.count()) > 0 &&
        (await recoveredFallbackTarget.first().isVisible())))
  ) {
    return;
  }

  throw new Error(
    `Entry content did not become ready for capture. Expected to see: "${expectedText}".`
  );
}

test.describe("Day One entry regression screenshots", () => {
  test.describe.configure({ timeout: 120_000 });

  for (const scenario of scenarios) {
    test(`${scenario.id}: ${scenario.description}`, async ({ page }) => {
      const targetUrl = resolveScenarioUrl(scenario.id);
      await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
      await page.waitForLoadState("load", { timeout: 10_000 }).catch(() => {});
      await ensureAuthenticated(page, targetUrl);
      await waitForScenarioReady(
        page,
        targetUrl,
        scenario.expectedText
      );

      await preparePageForStableCapture(page);
      await ensureAuthenticated(page, targetUrl);

      await expect(page).not.toHaveURL(/(login|signin)/i);
      await expect(page.getByText("404 - Page not found.")).toHaveCount(0);
      await captureScenario(page, scenario.id);
    });
  }
});
