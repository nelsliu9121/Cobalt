(() => {
  "use strict";
  const SECURE = "__Secure-next-auth.session-token";
  const INSECURE = "next-auth.session-token";
  const HEX = /^[0-9a-f]{64}$/;
  const NONCE = /^[A-Za-z0-9_-]{43}$/;

  function parseChallenge(fragment) {
    const match = /^#cobalt-login=v1\.([1-9][0-9]{0,4})\.([A-Za-z0-9_-]{43})$/.exec(fragment);
    if (!match || !NONCE.test(match[2])) return null;
    const port = Number(match[1]);
    return Number.isInteger(port) && port <= 65535
      ? { version: 1, port, nonce: match[2] }
      : null;
  }

  function validToken(value) {
    return value && typeof value === "object" && !Array.isArray(value)
      && typeof value.token === "string" && value.token.length > 0
      && !/[\u0000-\u001f\u007f]/.test(value.token)
      && Number.isSafeInteger(value.createdAt) && value.createdAt >= 0
      && Number.isSafeInteger(value.expiredAt) && value.expiredAt > value.createdAt;
  }

  async function authenticatedFingerprint(session) {
    const user = session && session.user;
    if (!user || typeof user !== "object" || Array.isArray(user)
      || !validToken(user.accessToken) || !validToken(user.refreshToken)) return null;
    const encoder = new TextEncoder();
    const access = encoder.encode(user.accessToken.token);
    const refresh = encoder.encode(user.refreshToken.token);
    const material = new Uint8Array(access.length + 1 + refresh.length);
    material.set(access);
    material[access.length] = 0;
    material.set(refresh, access.length + 1);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", material));
    return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  function member(name, family) {
    return typeof name === "string"
      && (name === family || name.startsWith(family + "."));
  }

  const bytes = (value) => new TextEncoder().encode(value).length;

  function filterSessionCookies(cookies) {
    if (!Array.isArray(cookies)) throw new Error("invalid cookie list");
    const selected = cookies.filter((cookie) =>
      cookie && typeof cookie === "object" && !Array.isArray(cookie)
      && (member(cookie.name, SECURE) || member(cookie.name, INSECURE))
    );
    if (selected.length > 16) throw new Error("too many session cookies");
    return selected.map((cookie) => {
      if (typeof cookie.value !== "string"
        || typeof cookie.domain !== "string"
        || typeof cookie.path !== "string"
        || typeof cookie.secure !== "boolean"
        || bytes(cookie.name) > 128
        || bytes(cookie.value) > 4096
        || bytes(cookie.domain) > 255
        || bytes(cookie.path) > 1024) {
        throw new Error("invalid session cookie");
      }
      const { name, value, domain, path, secure } = cookie;
      return { name, value, domain, path, secure };
    });
  }

  function payload(fingerprint, cookies) {
    if (!HEX.test(fingerprint) || cookies.length > 16) throw new Error("invalid handoff payload");
    const value = { version: 1, fingerprint, cookies };
    if (new TextEncoder().encode(JSON.stringify(value)).length > 16 * 1024) {
      throw new Error("handoff payload too large");
    }
    return value;
  }

  function endpoint(challenge) {
    return `http://127.0.0.1:${challenge.port}/bomtoon-login/${challenge.nonce}`;
  }

  function terminalStatus(status) {
    return status === 204 || status === 422;
  }

  globalThis.CobaltBomtoonProtocol = Object.freeze({
    parseChallenge, authenticatedFingerprint, filterSessionCookies,
    payload, endpoint, terminalStatus,
  });
})();
