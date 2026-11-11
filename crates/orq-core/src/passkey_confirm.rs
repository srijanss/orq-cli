//! Confirming one change with the passkey again (a step-up): a signed-in page asks for a challenge
//! for a named purpose, the person answers it with their passkey and user verification, and the
//! change goes ahead only on that answer. Each challenge is bound to its purpose and to the session
//! that asked for it, lasts two minutes, and is spent the first time it is presented.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::passkey_challenge::random_b64url;
use crate::passkey_login::{verify_assertion, AssertionResponse, Refused};
use crate::passkey_register::PasskeyHost;
use crate::passkey_store::PasskeyStore;

/// How long a confirmation challenge can be answered.
pub const CONFIRM_SECS: u64 = 120;

/// The changes that are confirmed with the passkey; a challenge is issued for no other purpose.
pub const PURPOSES: &[&str] = &["remote_actions.on"];

/// Most confirmation challenges kept at once; past this the oldest go first.
const MAX_PENDING: usize = 256;

struct Pending {
    purpose: String,
    session: String,
    issued: u64,
}

/// Confirmation challenges waiting for an answer, in memory only.
#[derive(Default)]
pub struct Confirmations {
    pending: Mutex<HashMap<String, Pending>>,
}

impl Confirmations {
    pub fn new() -> Confirmations {
        Confirmations::default()
    }

    /// A new challenge for `purpose`, asked for by the session `session`. `now` is seconds since the epoch.
    pub fn issue(&self, purpose: &str, session: &str, now: u64) -> Result<String, String> {
        let text = random_b64url(32)?;
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.retain(|_, entry| now.saturating_sub(entry.issued) < CONFIRM_SECS);
        while pending.len() >= MAX_PENDING {
            let Some(oldest) = pending.iter().min_by_key(|(_, entry)| entry.issued).map(|(c, _)| c.clone()) else { break };
            pending.remove(&oldest);
        }
        pending.insert(text.clone(), Pending { purpose: purpose.to_string(), session: session.to_string(), issued: now });
        Ok(text)
    }

    /// Spends `challenge`: true only if it was issued for `purpose` to `session`, is not expired, and
    /// has not been spent. It is gone afterwards either way.
    pub fn spend(&self, purpose: &str, session: &str, challenge: &str, now: u64) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match pending.remove(challenge) {
            Some(entry) => entry.purpose == purpose && entry.session == session && now.saturating_sub(entry.issued) < CONFIRM_SECS,
            None => false,
        }
    }
}

/// The options for `navigator.credentials.get`, with a challenge for `purpose` bound to `session`.
pub fn begin(host: &PasskeyHost, confirmations: &Confirmations, store: &PasskeyStore, purpose: &str, session: &str, now: u64) -> Result<Value, String> {
    let keys = store.list()?;
    if keys.is_empty() {
        return Err("no passkey is registered".into());
    }
    let allowed: Vec<Value> = keys.iter().map(|key| json!({"type": "public-key", "id": key.credential_id})).collect();
    Ok(json!({
        "challenge": confirmations.issue(purpose, session, now)?,
        "rpId": host.rp_id(),
        "userVerification": "required",
        "timeout": CONFIRM_SECS * 1000,
        "allowCredentials": allowed,
    }))
}

/// Whether `response` answers a challenge issued for `purpose` to `session`, with every check a
/// sign-in makes. The challenge is spent whatever the outcome.
pub fn check(host: &PasskeyHost, confirmations: &Confirmations, store: &PasskeyStore, purpose: &str, session: &str, response: &AssertionResponse, now: u64) -> Result<(), Refused> {
    verify_assertion(host, store, response, |challenge| confirmations.spend(purpose, session, challenge, now)).map(|_| ())
}

/// The assertion as the page sends it: `id`, `clientDataJSON`, `authenticatorData` and `signature`.
pub fn assertion_from(value: &Value) -> Option<AssertionResponse> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    Some(AssertionResponse { credential_id: text("id")?, client_data_json: text("clientDataJSON")?, authenticator_data: text("authenticatorData")?, signature: text("signature")? })
}
