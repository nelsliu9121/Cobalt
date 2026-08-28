importScripts("protocol.js");

(() => {
  "use strict";

  const { parseChallenge } = globalThis.CobaltBomtoonProtocol;

  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    let senderUrl;
    try {
      senderUrl = new URL(sender.tab.url);
    } catch {
      sendResponse({ accepted: false });
      return false;
    }

    if (!message || message.type !== "capture-challenge"
      || senderUrl.protocol !== "https:"
      || senderUrl.hostname !== "www.bomtoon.tw") {
      sendResponse({ accepted: false });
      return false;
    }

    const challenge = parseChallenge(message.fragment);
    if (!challenge) {
      sendResponse({ accepted: false });
      return false;
    }

    const value = {
      ...challenge,
      expiresAt: Date.now() + 600000,
    };
    chrome.storage.session.set({ bomtoonChallenge: value }).then(
      () => sendResponse({ accepted: true }),
      () => sendResponse({ accepted: false }),
    );
    return true;
  });
})();
