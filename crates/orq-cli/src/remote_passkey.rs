//! `orq remote passkey list|remove <label>`: manage the passkeys that sign people in to the inbox.
//! It acts on the passkey file on this machine, so it is also the way back in when every device
//! with a passkey is lost: removing the last one lets the token enrol a new one again.

use std::ffi::OsString;
use std::process::ExitCode;

use orq_core::passkey_store::PasskeyStore;
use orq_core::storage::StorageRoots;

use crate::{fail, usage_error};

pub fn passkey(args: &[OsString]) -> ExitCode {
    let words: Vec<&str> = args.iter().filter_map(|arg| arg.to_str()).collect();
    if words.len() != args.len() {
        return usage_error();
    }
    match words.as_slice() {
        ["list"] => with_store(|store| {
            let keys = store.list()?;
            if keys.is_empty() {
                println!("no passkeys are registered");
            }
            for key in keys {
                println!("{}\t{}", key.label, key.credential_id);
            }
            Ok(())
        }),
        ["remove", label] => with_store(|store| {
            store.remove(label)?;
            println!("removed the passkey {label}; sessions signed in with it end at once");
            Ok(())
        }),
        _ => usage_error(),
    }
}

fn with_store(action: impl FnOnce(&PasskeyStore) -> Result<(), String>) -> ExitCode {
    let roots = match StorageRoots::resolve(&|name| std::env::var(name).ok()) {
        Ok(roots) => roots,
        Err(err) => return fail(format!("cannot find the storage directories: {:?}", err.kind())),
    };
    match action(&PasskeyStore::open(&roots)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => fail(reason),
    }
}
