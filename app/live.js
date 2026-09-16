/**
 * Live devnet panel for demo.html.
 *
 * The cases on the page are a recorded engine run. This reads the chain: it
 * fetches a real claim account from devnet over plain JSON-RPC and decodes the
 * Anchor account layout by hand, so the page needs no wallet, no bundler and no
 * third-party script — which is also why it works as a static file on GitHub
 * Pages.
 *
 * Everything it renders goes in through textContent. Chain data is data, and a
 * panel that interpolated it into innerHTML would be one `location_hint` away
 * from executing whatever a submitter typed.
 */
(function (root) {
  "use strict";

  const DEVNET_RPC = "https://api.devnet.solana.com";
  const EXPLORER = "https://explorer.solana.com";

  // The claim this deployment submitted, challenged and confirmed on devnet.
  const LIVE_CLAIM = "9zhiCikXgU6RztSXgoGESFGttmMxyomZCA48Bn3QWuk2";

  const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

  function bs58Encode(bytes) {
    const digits = [0];
    for (const byte of bytes) {
      let carry = byte;
      for (let i = 0; i < digits.length; i++) {
        carry += digits[i] << 8;
        digits[i] = carry % 58;
        carry = (carry / 58) | 0;
      }
      while (carry > 0) {
        digits.push(carry % 58);
        carry = (carry / 58) | 0;
      }
    }
    let out = "";
    for (const byte of bytes) {
      if (byte !== 0) break;
      out += "1";
    }
    for (let i = digits.length - 1; i >= 0; i--) out += B58[digits[i]];
    return out;
  }

  // ClaimStatus, in the order the program declares it.
  const STATUS = [
    "pending",
    "challenged",
    "confirmed",
    "slashed",
    "finalized",
    "unresolved",
  ];

  /**
   * Decode a Claim account: 8-byte Anchor discriminator, then the fields in
   * declaration order, little-endian, with Option<T> as a 1-byte tag.
   */
  function decodeClaim(data) {
    const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
    let offset = 8;
    const take = (n) => {
      const slice = data.slice(offset, offset + n);
      offset += n;
      return slice;
    };
    const hex = (bytes) =>
      Array.from(bytes)
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");

    const submitter = bs58Encode(take(32));
    const asset = bs58Encode(take(32));
    const inputsHash = hex(take(32));
    const modelVersion = view.getUint32(offset, true);
    offset += 4;
    const periodStart = Number(view.getBigInt64(offset, true));
    offset += 8;
    const claimedCo2Kg = Number(view.getBigUint64(offset, true));
    offset += 8;
    const selfScoreBps = view.getUint16(offset, true);
    offset += 2;
    const bond = Number(view.getBigUint64(offset, true));
    offset += 8;
    const status = STATUS[data[offset]] || "unknown";
    offset += 1;
    const hasChallenger = data[offset] === 1;
    offset += 1;
    const challenger = hasChallenger ? bs58Encode(take(32)) : null;
    const createdAt = Number(view.getBigInt64(offset, true));
    offset += 8;
    offset += 1; // bump
    const hasResolved = data[offset] === 1;
    offset += 1;
    const committeeScoreBps = hasResolved ? view.getUint16(offset, true) : null;

    return {
      submitter,
      asset,
      inputsHash,
      modelVersion,
      periodStart,
      claimedCo2Kg,
      selfScoreBps,
      bond,
      status,
      challenger,
      createdAt,
      committeeScoreBps,
    };
  }

  async function fetchClaim(address = LIVE_CLAIM, rpc = DEVNET_RPC) {
    const response = await fetch(rpc, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "getAccountInfo",
        params: [address, { encoding: "base64", commitment: "confirmed" }],
      }),
    });
    const json = await response.json();
    const value = json && json.result && json.result.value;
    if (!value) return null;
    const binary = atob(value.data[0]);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return decodeClaim(bytes);
  }

  const shorten = (s) => `${s.slice(0, 6)}…${s.slice(-6)}`;
  const sol = (lamports) => `${(lamports / 1e9).toFixed(3)} SOL`;

  function row(label, valueNode) {
    const line = document.createElement("div");
    line.className = "live-row";
    const key = document.createElement("span");
    key.className = "live-key";
    key.textContent = label;
    const value = document.createElement("span");
    value.className = "live-val";
    if (typeof valueNode === "string") value.textContent = valueNode;
    else value.appendChild(valueNode);
    line.appendChild(key);
    line.appendChild(value);
    return line;
  }

  function link(text, href) {
    const a = document.createElement("a");
    a.textContent = text;
    a.href = href;
    a.target = "_blank";
    a.rel = "noopener";
    return a;
  }

  async function render(container, address = LIVE_CLAIM) {
    if (!container) return;
    container.textContent = "reading devnet…";
    let claim;
    try {
      claim = await fetchClaim(address);
    } catch (err) {
      container.textContent =
        "devnet is unreachable from here — the recorded cases above still run.";
      return;
    }
    if (!claim) {
      container.textContent = "this claim is not on devnet.";
      return;
    }

    container.textContent = "";
    container.appendChild(
      row(
        "claim",
        link(shorten(address), `${EXPLORER}/address/${address}?cluster=devnet`)
      )
    );
    container.appendChild(row("status", claim.status));
    container.appendChild(
      row("submitter's own score", `${claim.selfScoreBps} bps`)
    );
    container.appendChild(
      row(
        "committee score",
        claim.committeeScoreBps === null
          ? "not resolved yet"
          : `${claim.committeeScoreBps} bps`
      )
    );
    container.appendChild(row("claimed CO₂", `${claim.claimedCo2Kg} kg`));
    container.appendChild(row("bond held", sol(claim.bond)));
    container.appendChild(
      row(
        "asset",
        link(
          shorten(claim.asset),
          `${EXPLORER}/address/${claim.asset}?cluster=devnet`
        )
      )
    );
  }

  const api = { bs58Encode, decodeClaim, fetchClaim, render, LIVE_CLAIM, DEVNET_RPC };
  root.VeritasLive = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis !== "undefined" ? globalThis : this);
