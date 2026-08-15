#!/usr/bin/env node
// Pre-submission availability check for the public URLs the store listings
// cite (app landing page, legal pages, support, Cruise Pass, relay health,
// and the iOS universal-links file). No dependencies: uses global fetch.
//
// Usage: node tools/check_public_urls.mjs
// Exit code is nonzero if any check fails; a per-URL report is printed.

const TIMEOUT_MS = 15000;

/**
 * Each check hits a URL, requires HTTP 200, and asserts the body contains
 * a stable phrase (a content sanity check, not just "the server answered").
 * `validate` may be used instead of `phrase` for structured responses.
 */
const checks = [
  {
    url: "https://cruisemesh.app/",
    phrase: "Text your family when there's no signal.",
  },
  {
    url: "https://cruisemesh.app/privacy/",
    phrase: "Privacy Policy",
  },
  {
    url: "https://cruisemesh.app/terms/",
    phrase: "Terms of Use",
  },
  {
    url: "https://cruisemesh.app/support/",
    phrase: "Support",
  },
  {
    url: "https://cruisemesh.app/pass/",
    phrase: "One internet package. The whole family in touch.",
  },
  {
    url: "https://relay.cruisemesh.app/healthz",
    phrase: '"status":"ok"',
  },
  {
    url: "https://cruisemesh.app/.well-known/apple-app-site-association",
    validate: (body, url) => {
      let json;
      try {
        json = JSON.parse(body);
      } catch (err) {
        return `does not parse as JSON: ${err.message}`;
      }
      const appIds = json?.applinks?.details?.flatMap((d) => d.appIDs ?? []) ?? [];
      const found = appIds.some((id) => id.endsWith("com.cruisemesh.app"));
      if (!found) {
        return `parsed JSON but found no appID ending in "com.cruisemesh.app" (saw: ${JSON.stringify(appIds)})`;
      }
      return null; // pass
    },
  },
];

async function fetchWithTimeout(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(url, {
      redirect: "follow",
      signal: controller.signal,
      headers: { "user-agent": "cruisemesh-url-probe/1.0" },
    });
    const body = await res.text();
    return { status: res.status, body };
  } finally {
    clearTimeout(timer);
  }
}

async function runCheck(check) {
  const { url } = check;
  try {
    const { status, body } = await fetchWithTimeout(url);
    if (status !== 200) {
      return { url, ok: false, reason: `expected HTTP 200, got ${status}` };
    }
    if (check.validate) {
      const err = check.validate(body, url);
      if (err) {
        return { url, ok: false, reason: err };
      }
      return { url, ok: true };
    }
    if (!body.includes(check.phrase)) {
      return {
        url,
        ok: false,
        reason: `HTTP 200 but body did not contain expected phrase: ${JSON.stringify(check.phrase)}`,
      };
    }
    return { url, ok: true };
  } catch (err) {
    return { url, ok: false, reason: `request failed: ${err.message}` };
  }
}

async function main() {
  const results = await Promise.all(checks.map(runCheck));

  let failures = 0;
  for (const r of results) {
    if (r.ok) {
      console.log(`PASS  ${r.url}`);
    } else {
      failures++;
      console.log(`FAIL  ${r.url}`);
      console.log(`      ${r.reason}`);
    }
  }

  console.log("");
  console.log(`${results.length - failures}/${results.length} checks passed.`);

  if (failures > 0) {
    process.exitCode = 1;
  }
}

main();
