(() => {
  "use strict";

  const { parseChallenge, authenticatedFingerprint } = globalThis.CobaltBomtoonProtocol;
  const challenge = parseChallenge(location.hash);
  if (challenge) {
    chrome.runtime.sendMessage({
      type: "capture-challenge",
      fragment: location.hash,
    }).then((response) => {
      if (response && response.accepted === true) {
        history.replaceState(history.state, "", location.pathname + location.search);
      }
    }).catch(() => {});
  }

  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || message.type !== "session-fingerprint") return false;

    (async () => {
      try {
        const response = await fetch("https://www.bomtoon.tw/api/auth/session", {
          credentials: "same-origin",
          cache: "no-store",
          headers: { Accept: "application/json" },
        });
        if (!response.ok) {
          sendResponse({ authenticated: false });
          return;
        }
        const fingerprint = await authenticatedFingerprint(await response.json());
        sendResponse(fingerprint
          ? { authenticated: true, fingerprint }
          : { authenticated: false });
      } catch {
        sendResponse({ authenticated: false });
      }
    })();
    return true;
  });
})();
