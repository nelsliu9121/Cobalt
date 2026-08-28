const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const root = path.resolve(__dirname, "..");
const context = vm.createContext({
  TextEncoder,
  URL,
  crypto: require("node:crypto").webcrypto,
});
vm.runInContext(fs.readFileSync(path.join(root, "protocol.js"), "utf8"), context);
const protocol = context.CobaltBomtoonProtocol;
const plain = (value) => JSON.parse(JSON.stringify(value));

const nonce = "A".repeat(43);

const tick = () => new Promise((resolve) => setImmediate(resolve));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, resolve, reject };
}

function scriptContext(values = {}) {
  return vm.createContext({
    TextEncoder,
    URL,
    crypto: require("node:crypto").webcrypto,
    ...values,
  });
}

function runScript(target, name) {
  vm.runInContext(fs.readFileSync(path.join(root, name), "utf8"), target);
}

function loadProtocol(target) {
  runScript(target, "protocol.js");
}

function popupHarness(overrides = {}) {
  const handlers = {};
  const elements = Object.fromEntries(["status", "send", "cancel"].map((id) => [id, {
    disabled: false,
    textContent: "",
    addEventListener(type, handler) {
      handlers[`${id}:${type}`] = handler;
    },
  }]));
  const clock = { now: overrides.now === undefined ? Date.now() : overrides.now };
  class HarnessDate extends Date {
    static now() {
      return clock.now;
    }
  }
  const removals = [];
  const cookieReads = [];
  const tabQueries = [];
  const tabMessages = [];
  const fetches = [];
  const challenge = overrides.challenge === undefined
    ? { version: 1, port: 43125, nonce, expiresAt: clock.now + 600000 }
    : overrides.challenge;
  const tab = overrides.tab || { id: 7, url: "https://www.bomtoon.tw/library" };
  const chrome = {
    storage: {
      session: {
        async get(key) {
          assert.equal(key, "bomtoonChallenge");
          return challenge ? { bomtoonChallenge: challenge } : {};
        },
        async remove(key) {
          removals.push(key);
        },
      },
    },
    tabs: {
      async query(query) {
        tabQueries.push(plain(query));
        assert.deepEqual(plain(query), { active: true, currentWindow: true });
        return [tab];
      },
      async sendMessage(tabId, message) {
        tabMessages.push({ tabId, message });
        return overrides.sessionResponse || {
          authenticated: true,
          fingerprint: "a".repeat(64),
        };
      },
    },
    cookies: {
      async getAllCookieStores() {
        return overrides.stores || [{ id: "active-store", tabIds: [7] }];
      },
      async getAll(details) {
        cookieReads.push(details);
        return overrides.cookies || [];
      },
    },
  };
  const fetchImpl = overrides.fetch || (async () => ({ status: 204 }));
  let closes = 0;
  const target = scriptContext({
    Date: HarnessDate,
    chrome,
    document: { getElementById: (id) => elements[id] },
    fetch: async (url, options) => {
      fetches.push({ url, options });
      return fetchImpl(url, options);
    },
    close: () => { closes += 1; },
    console: { log: () => { throw new Error("must not log secrets"); } },
  });
  loadProtocol(target);
  runScript(target, "popup.js");
  return {
    elements,
    handlers,
    removals,
    cookieReads,
    tabQueries,
    tabMessages,
    fetches,
    advanceTo(value) { clock.now = value; },
    get closes() { return closes; },
  };
}

test("challenge grammar is exact", () => {
  assert.deepEqual(
    plain(protocol.parseChallenge(`#cobalt-login=v1.43125.${nonce}`)),
    { version: 1, port: 43125, nonce },
  );
  for (const value of [
    "", "#cobalt-login=v2.43125." + nonce,
    "#cobalt-login=v1.0." + nonce,
    "#cobalt-login=v1.65536." + nonce,
    "#cobalt-login=v1.43125.short",
    "#cobalt-login=v1.43125." + "A".repeat(42) + "/",
  ]) assert.equal(protocol.parseChallenge(value), null);
});

test("authenticated session produces the access-NUL-refresh digest", async () => {
  const session = {
    user: {
      accessToken: { token: "access", createdAt: 1, expiredAt: 2 },
      refreshToken: { token: "refresh", createdAt: 1, expiredAt: 3 },
    },
  };
  assert.equal(
    await protocol.authenticatedFingerprint(session),
    "f9a78e1bd56546fabfa615264de77c95eca9fea9bf87d5b9bc72a4fc1887b237",
  );
  for (const rejected of [null, {}, { user: {} }, {
    user: {
      accessToken: { token: "", createdAt: 1, expiredAt: 2 },
      refreshToken: { token: "refresh", createdAt: 1, expiredAt: 3 },
    },
  }]) assert.equal(await protocol.authenticatedFingerprint(rejected), null);
});

test("cookie filtering preserves NextAuth candidates for native validation", () => {
  const cookies = protocol.filterSessionCookies([
    { name: "__Secure-next-auth.session-token.0", value: "a", domain: ".bomtoon.tw", path: "/", secure: true },
    { name: "__Secure-next-auth.session-token.01", value: "bad", domain: ".bomtoon.tw", path: "/", secure: true },
    { name: "next-auth.csrf-token", value: "secret", domain: ".bomtoon.tw", path: "/", secure: true },
  ]);
  assert.deepEqual(plain(cookies), [
    { name: "__Secure-next-auth.session-token.0", value: "a", domain: ".bomtoon.tw", path: "/", secure: true },
    { name: "__Secure-next-auth.session-token.01", value: "bad", domain: ".bomtoon.tw", path: "/", secure: true },
  ]);
  assert.throws(() => protocol.filterSessionCookies(
    Array(17).fill(null).map((_, index) => ({
      name: `next-auth.session-token.${index}`,
      value: "x", domain: ".bomtoon.tw", path: "/", secure: true,
    })),
  ));
});

test("payload and endpoint are bounded", () => {
  const challenge = { version: 1, port: 43125, nonce };
  assert.equal(protocol.endpoint(challenge), `http://127.0.0.1:43125/bomtoon-login/${nonce}`);
  assert.deepEqual(plain(protocol.payload("a".repeat(64), [])), {
    version: 1,
    fingerprint: "a".repeat(64),
    cookies: [],
  });
  assert.throws(() => protocol.payload("A".repeat(64), []));
  assert.throws(() => protocol.payload("a".repeat(64), Array(17).fill({})));
});

test("only native terminal responses consume the challenge", () => {
  assert.equal(protocol.terminalStatus(204), true);
  assert.equal(protocol.terminalStatus(422), true);
  assert.equal(protocol.terminalStatus(400), false);
  assert.equal(protocol.terminalStatus(500), false);
});

test("manifest exposes only the required MV3 permissions and hosts", () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, "manifest.json"), "utf8"));
  assert.deepEqual(manifest, {
    manifest_version: 3,
    name: "Cobalt BOMTOON Login",
    version: "1.0.0",
    description: "Transfers one attended BOMTOON sign-in to the local Cobalt CLI.",
    permissions: ["activeTab", "cookies", "storage"],
    host_permissions: [
      "https://www.bomtoon.tw/*",
      "https://*.bomtoon.tw/*",
      "http://127.0.0.1/*",
    ],
    background: { service_worker: "background.js" },
    action: { default_popup: "popup.html" },
    content_scripts: [{
      matches: ["https://www.bomtoon.tw/*"],
      js: ["protocol.js", "content.js"],
      run_at: "document_start",
    }],
  });
});

test("background acknowledges a valid challenge only after session storage resolves", async () => {
  const stored = deferred();
  const writes = [];
  const listeners = [];
  const target = scriptContext({
    chrome: {
      runtime: { onMessage: { addListener: (listener) => listeners.push(listener) } },
      storage: { session: { set: (value) => { writes.push(value); return stored.promise; } } },
    },
    importScripts: (...names) => names.forEach((name) => runScript(target, name)),
  });
  runScript(target, "background.js");
  const replies = [];
  const startedAt = Date.now();
  assert.equal(listeners[0](
    { type: "capture-challenge", fragment: `#cobalt-login=v1.43125.${nonce}` },
    { tab: { url: "https://www.bomtoon.tw/library" } },
    (reply) => replies.push(reply),
  ), true);
  assert.deepEqual(replies, []);
  assert.equal(writes.length, 1);
  const saved = plain(writes[0].bomtoonChallenge);
  assert.deepEqual({ version: saved.version, port: saved.port, nonce: saved.nonce }, {
    version: 1, port: 43125, nonce,
  });
  assert.ok(saved.expiresAt >= startedAt + 600000);
  assert.ok(saved.expiresAt <= Date.now() + 600000);
  stored.resolve();
  await tick();
  assert.deepEqual(plain(replies), [{ accepted: true }]);

  const rejected = [];
  assert.equal(listeners[0](
    { type: "capture-challenge", fragment: `#cobalt-login=v1.43125.${nonce}` },
    { tab: { url: "https://accounts.google.com/" } },
    (reply) => rejected.push(reply),
  ), false);
  assert.deepEqual(plain(rejected), [{ accepted: false }]);
  assert.equal(writes.length, 1);
});

test("content removes the challenge fragment only after background acknowledgement", async () => {
  const acknowledgement = deferred();
  const messages = [];
  const replacements = [];
  const listeners = [];
  const location = {
    hash: `#cobalt-login=v1.43125.${nonce}`,
    pathname: "/library",
    search: "?from=cli",
  };
  const target = scriptContext({
    location,
    history: {
      state: { page: 1 },
      replaceState: (...args) => replacements.push(args),
    },
    chrome: {
      runtime: {
        sendMessage: (message) => { messages.push(message); return acknowledgement.promise; },
        onMessage: { addListener: (listener) => listeners.push(listener) },
      },
    },
    fetch: async () => { throw new Error("not requested"); },
  });
  loadProtocol(target);
  runScript(target, "content.js");
  assert.deepEqual(plain(messages), [{
    type: "capture-challenge",
    fragment: `#cobalt-login=v1.43125.${nonce}`,
  }]);
  assert.deepEqual(replacements, []);
  acknowledgement.resolve({ accepted: true });
  await tick();
  assert.deepEqual(plain(replacements), [[{ page: 1 }, "", "/library?from=cli"]]);
  assert.equal(listeners.length, 1);
});

test("content returns only an authenticated fingerprint for the page session", async () => {
  const listeners = [];
  const requests = [];
  const session = {
    user: {
      accessToken: { token: "access", createdAt: 1, expiredAt: 2 },
      refreshToken: { token: "refresh", createdAt: 1, expiredAt: 3 },
    },
  };
  const target = scriptContext({
    location: { hash: "", pathname: "/", search: "" },
    history: { state: null, replaceState: () => {} },
    chrome: {
      runtime: {
        sendMessage: async () => ({ accepted: false }),
        onMessage: { addListener: (listener) => listeners.push(listener) },
      },
    },
    fetch: async (url, options) => {
      requests.push({ url, options });
      return { ok: true, json: async () => session };
    },
  });
  loadProtocol(target);
  runScript(target, "content.js");
  const replies = [];
  assert.equal(listeners[0]({ type: "session-fingerprint" }, {}, (reply) => replies.push(reply)), true);
  await tick();
  assert.deepEqual(plain(requests), [{
    url: "https://www.bomtoon.tw/api/auth/session",
    options: {
      credentials: "same-origin",
      cache: "no-store",
      headers: { Accept: "application/json" },
    },
  }]);
  assert.deepEqual(plain(replies), [{
    authenticated: true,
    fingerprint: "f9a78e1bd56546fabfa615264de77c95eca9fea9bf87d5b9bc72a4fc1887b237",
  }]);
  assert.equal(JSON.stringify(replies).includes("access"), false);
  assert.equal(JSON.stringify(replies).includes("refresh"), false);
});

test("popup wrong-tab and signed-out states do not read cookies", async () => {
  const wrongTab = popupHarness({ tab: { id: 7, url: "https://example.com/" } });
  await tick();
  await wrongTab.handlers["send:click"]();
  assert.equal(wrongTab.cookieReads.length, 0);
  assert.equal(wrongTab.tabMessages.length, 0);
  assert.equal(wrongTab.removals.length, 0);

  const signedOut = popupHarness({ sessionResponse: { authenticated: false } });
  await tick();
  await signedOut.handlers["send:click"]();
  assert.equal(signedOut.cookieReads.length, 0);
  assert.deepEqual(plain(signedOut.tabMessages), [{
    tabId: 7,
    message: { type: "session-fingerprint" },
  }]);
  assert.equal(signedOut.removals.length, 0);
});

test("popup uses the active tab cookie store, filters payload, and clears on 204", async () => {
  const sessionCookie = {
    name: "__Secure-next-auth.session-token.0",
    value: "session-secret",
    domain: ".bomtoon.tw",
    path: "/",
    secure: true,
    httpOnly: true,
  };
  const harness = popupHarness({
    stores: [
      { id: "other-store", tabIds: [99] },
      { id: "active-store", tabIds: [7, 8] },
    ],
    cookies: [
      sessionCookie,
      { name: "next-auth.csrf-token", value: "csrf-secret", domain: ".bomtoon.tw", path: "/", secure: true },
    ],
  });
  await tick();
  await harness.handlers["send:click"]();
  assert.deepEqual(plain(harness.cookieReads), [{
    url: "https://www.bomtoon.tw/",
    storeId: "active-store",
  }]);
  assert.equal(harness.fetches.length, 1);
  const request = harness.fetches[0];
  assert.equal(request.url, `http://127.0.0.1:43125/bomtoon-login/${nonce}`);
  assert.deepEqual(plain({ ...request.options, body: undefined }), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "omit",
    cache: "no-store",
    redirect: "error",
  });
  assert.deepEqual(JSON.parse(request.options.body), {
    version: 1,
    fingerprint: "a".repeat(64),
    cookies: [{
      name: sessionCookie.name,
      value: sessionCookie.value,
      domain: sessionCookie.domain,
      path: sessionCookie.path,
      secure: sessionCookie.secure,
    }],
  });
  assert.equal(request.options.body.includes("csrf-secret"), false);
  assert.deepEqual(harness.removals, ["bomtoonChallenge"]);
});

test("popup revalidates expiry before starting credential work", async () => {
  const harness = popupHarness({
    now: 1000,
    challenge: { version: 1, port: 43125, nonce, expiresAt: 2000 },
  });
  await tick();
  assert.equal(harness.elements.send.disabled, false);
  harness.advanceTo(2001);
  await harness.handlers["send:click"]();
  assert.deepEqual(harness.removals, ["bomtoonChallenge"]);
  assert.equal(harness.elements.send.disabled, true);
  assert.equal(harness.elements.status.textContent, "Run kobo bomtoon login first.");
  assert.deepEqual(harness.tabQueries, []);
  assert.deepEqual(harness.tabMessages, []);
  assert.deepEqual(harness.cookieReads, []);
  assert.deepEqual(harness.fetches, []);
});

test("popup revalidates expiry after awaited credential work", async () => {
  const cookies = deferred();
  const harness = popupHarness({
    now: 1000,
    challenge: { version: 1, port: 43125, nonce, expiresAt: 2000 },
    cookies: cookies.promise,
  });
  await tick();
  const handoff = harness.handlers["send:click"]();
  await tick();
  assert.equal(harness.cookieReads.length, 1);
  harness.advanceTo(2001);
  cookies.resolve([]);
  await handoff;
  assert.deepEqual(harness.removals, ["bomtoonChallenge"]);
  assert.equal(harness.elements.send.disabled, true);
  assert.equal(harness.elements.status.textContent, "Run kobo bomtoon login first.");
  assert.deepEqual(harness.fetches, []);
});

test("popup keeps the challenge after a failed POST and allows retry", async () => {
  let attempts = 0;
  const harness = popupHarness({
    fetch: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("listener unavailable");
      return { status: 204 };
    },
  });
  await tick();
  await harness.handlers["send:click"]();
  assert.equal(attempts, 1);
  assert.deepEqual(harness.removals, []);
  assert.equal(harness.elements.send.disabled, false);
  await harness.handlers["send:click"]();
  assert.equal(attempts, 2);
  assert.deepEqual(harness.removals, ["bomtoonChallenge"]);
});

test("popup clears an expired challenge and cancel clears and closes", async () => {
  const expired = popupHarness({
    challenge: { version: 1, port: 43125, nonce, expiresAt: Date.now() - 1 },
  });
  await tick();
  assert.deepEqual(expired.removals, ["bomtoonChallenge"]);
  assert.equal(expired.elements.status.textContent, "Run kobo bomtoon login first.");

  const active = popupHarness();
  await tick();
  await active.handlers["cancel:click"]();
  assert.deepEqual(active.removals, ["bomtoonChallenge"]);
  assert.equal(active.closes, 1);
});
