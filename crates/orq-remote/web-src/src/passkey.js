// Passkey calls to the server's /auth routes and the browser's WebAuthn API. Binary values travel
// as base64url text. Nothing here keeps a secret: the session lives in an HttpOnly cookie.

function toBytes(text) {
  const padded = text.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (text.length % 4)) % 4);
  const raw = atob(padded);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

function toText(buffer) {
  const bytes = new Uint8Array(buffer);
  let raw = "";
  for (let i = 0; i < bytes.length; i += 1) raw += String.fromCharCode(bytes[i]);
  return btoa(raw).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// Why this page cannot use passkeys, in words for the person reading it; null when it can.
export function passkeyBlocker() {
  if (typeof window === "undefined" || window.isSecureContext !== true) return "Passkeys need an HTTPS connection to this page.";
  if (typeof window.PublicKeyCredential !== "function" || !navigator.credentials) return "This browser cannot use passkeys.";
  return null;
}

export function passkeysAvailable() {
  return passkeyBlocker() === null;
}

function post(path, body, token) {
  const headers = { "Content-Type": "application/json" };
  if (typeof token === "string") headers.Authorization = "Bearer " + token;
  return fetch(path, { method: "POST", headers, body: JSON.stringify(body ?? {}), cache: "no-store", referrerPolicy: "no-referrer" });
}

// What the server says about sign-in; null when it has no passkey support.
export async function authStatus() {
  try {
    const response = await fetch("/auth/status", { cache: "no-store", referrerPolicy: "no-referrer" });
    if (!response.ok) return null;
    const data = await response.json();
    return data !== null && typeof data === "object" && typeof data.passkeys === "boolean" ? data : null;
  } catch (unavailable) {
    return null;
  }
}

// The passkeys registered on the server, as [{label, id}]. null when this page holds no session
// (the server refuses or has no passkeys); "failed" when the list could not be read.
export async function listPasskeys() {
  try {
    const response = await fetch("/auth/passkeys", { cache: "no-store", referrerPolicy: "no-referrer" });
    if (response.status === 401 || response.status === 404) return null;
    if (!response.ok) return "failed";
    const data = await response.json();
    return data !== null && Array.isArray(data.passkeys) ? data.passkeys.filter((k) => typeof k.label === "string" && typeof k.id === "string") : "failed";
  } catch (unavailable) {
    return "failed";
  }
}

// Signs in with a stored passkey. Resolves true when the server set the session cookie.
export async function signInWithPasskey() {
  const began = await post("/auth/login/begin");
  if (!began.ok) return false;
  const options = await began.json();
  const credential = await navigator.credentials.get({
    publicKey: {
      challenge: toBytes(options.challenge),
      rpId: options.rpId,
      userVerification: options.userVerification,
      timeout: options.timeout,
      allowCredentials: options.allowCredentials.map((entry) => ({ type: entry.type, id: toBytes(entry.id) })),
    },
  });
  if (!credential) return false;
  const answer = credential.response;
  const finished = await post("/auth/login/finish", {
    id: toText(credential.rawId),
    clientDataJSON: toText(answer.clientDataJSON),
    authenticatorData: toText(answer.authenticatorData),
    signature: toText(answer.signature),
  });
  return finished.ok;
}

// Adds a passkey. The first one is allowed with the access token; later ones from a signed-in
// session (token is then null and the cookie speaks for the page).
export async function registerPasskey(token, label) {
  const began = await post("/auth/register/begin", {}, token);
  if (!began.ok) return false;
  const options = await began.json();
  const credential = await navigator.credentials.create({
    publicKey: {
      challenge: toBytes(options.challenge),
      rp: options.rp,
      user: { id: toBytes(options.user.id), name: options.user.name, displayName: options.user.displayName },
      pubKeyCredParams: options.pubKeyCredParams,
      authenticatorSelection: options.authenticatorSelection,
      attestation: options.attestation,
      timeout: options.timeout,
      excludeCredentials: options.excludeCredentials.map((entry) => ({ type: entry.type, id: toBytes(entry.id) })),
    },
  });
  if (!credential) return false;
  const finished = await post(
    "/auth/register/finish",
    { clientDataJSON: toText(credential.response.clientDataJSON), attestationObject: toText(credential.response.attestationObject), label },
    token,
  );
  return finished.ok;
}

export async function signOutOfSession() {
  try {
    await post("/auth/logout");
  } catch (unavailable) {
    // the cookie expires on its own
  }
}
