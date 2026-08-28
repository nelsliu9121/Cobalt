(() => {
  "use strict";

  const {
    filterSessionCookies,
    payload,
    endpoint,
    terminalStatus,
  } = globalThis.CobaltBomtoonProtocol;
  const status = document.getElementById("status");
  const sendButton = document.getElementById("send");
  const cancelButton = document.getElementById("cancel");
  let challenge = null;
  let sending = false;

  function show(message) {
    status.textContent = message;
  }

  async function clearChallenge() {
    await chrome.storage.session.remove("bomtoonChallenge");
    challenge = null;
  }

  async function initialize() {
    const stored = await chrome.storage.session.get("bomtoonChallenge");
    const candidate = stored.bomtoonChallenge;
    if (!candidate || !Number.isFinite(candidate.expiresAt)
      || candidate.expiresAt <= Date.now()) {
      await clearChallenge();
      sendButton.disabled = true;
      show("Run kobo bomtoon login first.");
      return;
    }
    challenge = candidate;
    sendButton.disabled = false;
    show("Ready to send BOMTOON sign-in.");
  }

  const ready = initialize().catch(async () => {
    try {
      await clearChallenge();
    } catch {
      challenge = null;
    }
    sendButton.disabled = true;
    show("Run kobo bomtoon login first.");
  });

  async function sendHandoff() {
    await ready;
    if (!challenge || sending) return;

    sending = true;
    sendButton.disabled = true;
    show("Sending BOMTOON sign-in…");
    try {
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      let tabUrl;
      try {
        tabUrl = new URL(tab.url);
      } catch {
        tabUrl = null;
      }
      if (!tab || !Number.isInteger(tab.id)
        || !tabUrl || tabUrl.origin !== "https://www.bomtoon.tw") {
        show("Open https://www.bomtoon.tw/ in the active tab.");
        return;
      }

      const session = await chrome.tabs.sendMessage(tab.id, {
        type: "session-fingerprint",
      });
      if (!session || session.authenticated !== true) {
        show("Sign in to BOMTOON first.");
        return;
      }

      const stores = await chrome.cookies.getAllCookieStores();
      const store = stores.find((candidate) =>
        Array.isArray(candidate.tabIds) && candidate.tabIds.includes(tab.id)
      );
      if (!store) {
        show("Could not access the active tab's cookies.");
        return;
      }

      const allCookies = await chrome.cookies.getAll({
        url: "https://www.bomtoon.tw/",
        storeId: store.id,
      });
      const cookies = filterSessionCookies(allCookies);
      const body = payload(session.fingerprint, cookies);
      const response = await fetch(endpoint(challenge), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        credentials: "omit",
        cache: "no-store",
        redirect: "error",
      });

      if (terminalStatus(response.status)) {
        await clearChallenge();
        sendButton.disabled = true;
        show(response.status === 204
          ? "BOMTOON sign-in sent to Kobo."
          : "Kobo rejected this sign-in. Run kobo bomtoon login again.");
      } else if (response.status === 400) {
        show("Kobo did not accept the sign-in. Try again.");
      } else {
        show("Kobo returned an unexpected response. Try again.");
      }
    } catch {
      show("Could not send the sign-in. Try again.");
    } finally {
      sending = false;
      if (challenge) sendButton.disabled = false;
    }
  }

  sendButton.addEventListener("click", sendHandoff);
  cancelButton.addEventListener("click", async () => {
    try {
      await clearChallenge();
    } finally {
      close();
    }
  });
})();
