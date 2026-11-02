//! Database key management.
//!
//! Signal Desktop generates a random SQLCipher key and protects it with the
//! OS credential store (Electron `safeStorage`). We do the same: a random
//! 256-bit key lives in the macOS Keychain, one item per account. Elsewhere
//! it falls back to a `0600` key file next to the databases.

use std::path::Path;

use anyhow::{Context as _, Result};
use rand::RngCore as _;

#[cfg(any(target_os = "macos", test))]
use crate::former_name::FORMER;
use crate::former_name::Former;

const SERVICE: &str = "vernam";
/// The Keychain item's account name: the app's first account's (from
/// before there were several), and the others' with their id after it.
const ACCOUNT: &str = "database-key";

/// Which account's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyItem {
    /// Its Keychain item's account name.
    account: String,
    /// The account the app had before there were several: its key may
    /// still be in a former name's item.
    legacy: bool,
}

impl KeyItem {
    /// The key of the account the app had before there were several.
    pub fn legacy() -> Self {
        Self { account: ACCOUNT.into(), legacy: true }
    }

    /// The key of the account with this id.
    pub fn account(id: &str) -> Self {
        Self { account: format!("{ACCOUNT}.{id}"), legacy: false }
    }
}

/// Where the key lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStore {
    /// The OS credential store (macOS Keychain); file fallback elsewhere.
    System,
    /// A `0600` file in the data directory. Used for development profiles
    /// (`SIGNAL_GPUI_DATA_DIR`) so they don't touch the login Keychain.
    File,
}

/// A database key, and the former name's Keychain item it came from, if
/// it did: copied to [`SERVICE`] only once it has opened the data
/// ([`keep`]), so a wrong guess can't replace a right one there.
pub struct Loaded {
    pub key: [u8; 32],
    copy_from: Option<&'static str>,
}

/// Loads the database key, creating and storing a new one on first run.
/// `moved_from`: the former name whose data just moved to `data_dir`, so
/// its key is the one that opens it.
pub fn load_or_create(data_dir: &Path, item: &KeyItem, store_kind: KeyStore, moved_from: Option<&'static Former>) -> Result<Loaded> {
    let existing = match store_kind {
        KeyStore::System => load(data_dir, item, moved_from)?,
        KeyStore::File => load_file(data_dir)?.map(|key| Loaded { key, copy_from: None }),
    };
    if let Some(loaded) = existing {
        return Ok(loaded);
    }
    // A new key can't open databases made with another: refuse rather than
    // store one, which would also hide a former key restored later.
    if let Some(db) = ["signal.db", "app.db"].iter().map(|name| data_dir.join(name)).find(|path| path.exists()) {
        anyhow::bail!(
            "found no database key for {}, which can't be opened without it. Only if that key is gone for \
             good: deleting {} starts over, unlinking this device and losing what's stored there",
            db.display(),
            data_dir.display()
        );
    }
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    // Kept before any database exists: one made without its key kept
    // couldn't be opened again.
    match store_kind {
        KeyStore::System => store(data_dir, item, &key)?,
        KeyStore::File => store_file(data_dir, &key)?,
    }
    Ok(Loaded { key, copy_from: None })
}

/// Forgets a removed account's key (its data is gone). A key file goes
/// with its directory.
pub fn forget(item: &KeyItem, store_kind: KeyStore) {
    if store_kind == KeyStore::System {
        delete(item);
    }
}

#[cfg(target_os = "macos")]
fn delete(item: &KeyItem) {
    match security_framework::passwords::delete_generic_password(SERVICE, &item.account) {
        Ok(()) => {}
        // errSecItemNotFound
        Err(e) if e.code() == -25300 => {}
        Err(error) => tracing::warn!(%error, "couldn't delete the database key {:?}", item.account),
    }
}

#[cfg(not(target_os = "macos"))]
fn delete(_item: &KeyItem) {}

/// The key opened the data: one from a former name's item is copied to
/// [`SERVICE`], and left in that item too, so that app can still read it.
pub fn keep(data_dir: &Path, item: &KeyItem, loaded: &Loaded) -> Result<()> {
    if let Some(service) = loaded.copy_from {
        store(data_dir, item, &loaded.key)?;
        tracing::info!("copied the database key from the Keychain item {service:?} to {SERVICE:?}");
    }
    Ok(())
}

fn decode(hex_key: &[u8]) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex_key.trim_ascii()).context("stored database key is not hex")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("stored database key has the wrong length"))
}

/// The key: for data that just moved in, its former name's, then the other
/// former names', newest first, and Vernam's last (a `vernam` key from
/// before is for a profile of Vernam's own that's gone: the data moved in
/// only because Vernam's directory didn't exist). Otherwise Vernam's, then
/// the former names', newest first, so there's never a new key for
/// databases an old name's key encrypts. Only the legacy account's key can
/// be in a former name's item: an account added since has its own.
#[cfg(target_os = "macos")]
fn load(_data_dir: &Path, item: &KeyItem, moved_from: Option<&'static Former>) -> Result<Option<Loaded>> {
    if !item.legacy {
        return Ok(load_service(SERVICE, &item.account)?.map(|key| Loaded { key, copy_from: None }));
    }
    let found = find(moved_from, |service| load_service(service, ACCOUNT))?;
    Ok(found.map(|(service, key)| Loaded { key, copy_from: (service != SERVICE).then_some(service) }))
}

/// The first key there is, in [`load`]'s order, and the item it's in.
#[cfg(any(target_os = "macos", test))]
fn find(
    moved_from: Option<&'static Former>,
    mut lookup: impl FnMut(&'static str) -> Result<Option<[u8; 32]>>,
) -> Result<Option<(&'static str, [u8; 32])>> {
    let others = FORMER.iter().filter(|former| Some(*former) != moved_from).map(|former| former.keychain_service);
    let services: Vec<&'static str> = match moved_from {
        Some(former) => [former.keychain_service].into_iter().chain(others).chain([SERVICE]).collect(),
        None => [SERVICE].into_iter().chain(others).collect(),
    };
    for service in services {
        if let Some(key) = lookup(service)? {
            return Ok(Some((service, key)));
        }
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn load_service(service: &str, account: &str) -> Result<Option<[u8; 32]>> {
    use security_framework::passwords::get_generic_password;
    match get_generic_password(service, account) {
        Ok(bytes) => decode(&bytes).map(Some),
        // errSecItemNotFound
        Err(e) if e.code() == -25300 => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading database key {service:?} from the Keychain")),
    }
}

#[cfg(target_os = "macos")]
fn store(_data_dir: &Path, item: &KeyItem, key: &[u8; 32]) -> Result<()> {
    security_framework::passwords::set_generic_password(SERVICE, &item.account, hex::encode(key).as_bytes())
        .context("storing database key in the Keychain")
}

#[cfg(not(target_os = "macos"))]
fn load(data_dir: &Path, _item: &KeyItem, _moved_from: Option<&'static Former>) -> Result<Option<Loaded>> {
    Ok(load_file(data_dir)?.map(|key| Loaded { key, copy_from: None }))
}

#[cfg(not(target_os = "macos"))]
fn store(data_dir: &Path, _item: &KeyItem, key: &[u8; 32]) -> Result<()> {
    store_file(data_dir, key)
}

fn load_file(data_dir: &Path) -> Result<Option<[u8; 32]>> {
    match std::fs::read(data_dir.join("db.key")) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("reading database key"),
    }
}

fn store_file(data_dir: &Path, key: &[u8; 32]) -> Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(data_dir.join("db.key")).context("creating database key file")?;
    file.write_all(hex::encode(key).as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Looks keys up in `items` (service, key byte), noting each lookup.
    fn keychain<'a>(
        items: &'a [(&str, u8)],
        asked: &'a mut Vec<&'static str>,
    ) -> impl FnMut(&'static str) -> Result<Option<[u8; 32]>> + 'a {
        move |service| {
            asked.push(service);
            Ok(items.iter().find(|(s, _)| *s == service).map(|(_, byte)| [*byte; 32]))
        }
    }

    #[test]
    fn data_that_moved_in_takes_its_apps_key() {
        let ziggy = &FORMER[0];
        // A `vernam` item from a profile of Vernam's own that's gone loses.
        let items = [("vernam", 1), ("ziggy", 2), ("signal-gpui", 3)];
        let mut asked = Vec::new();
        assert_eq!(find(Some(ziggy), keychain(&items, &mut asked)).unwrap(), Some(("ziggy", [2; 32])));
        // With no `ziggy` item (a denied prompt when Ziggy moved Signal
        // GPUI's data), older names come before a stale `vernam` one.
        let items = [("vernam", 1), ("signal-gpui", 3)];
        let mut asked = Vec::new();
        assert_eq!(find(Some(ziggy), keychain(&items, &mut asked)).unwrap(), Some(("signal-gpui", [3; 32])));
        assert_eq!(asked, ["ziggy", "signal-gpui"]);
        // Vernam's last, each asked once.
        let mut asked = Vec::new();
        assert_eq!(find(Some(ziggy), keychain(&[("vernam", 1)], &mut asked)).unwrap(), Some(("vernam", [1; 32])));
        assert_eq!(asked, ["ziggy", "signal-gpui", "vernam"]);
    }

    #[test]
    fn no_new_key_for_databases_that_exist() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("signal.db"), "encrypted").unwrap();
        let item = KeyItem::account("0a1b");
        let error = load_or_create(dir.path(), &item, KeyStore::File, None).err().unwrap().to_string();
        assert!(error.contains("found no database key"), "{error}");
        assert!(!dir.path().join("db.key").exists());
        // A new profile gets one, and keeps it.
        let fresh = tempfile::tempdir().unwrap();
        let key = load_or_create(fresh.path(), &item, KeyStore::File, None).unwrap().key;
        std::fs::write(fresh.path().join("signal.db"), "encrypted").unwrap();
        assert_eq!(load_or_create(fresh.path(), &item, KeyStore::File, None).unwrap().key, key);
    }

    #[test]
    fn otherwise_vernams_key_then_the_newest_former_names() {
        let items = [("vernam", 1), ("ziggy", 2)];
        assert_eq!(find(None, keychain(&items, &mut Vec::new())).unwrap(), Some(("vernam", [1; 32])));
        let items = [("signal-gpui", 3), ("ziggy", 2)];
        assert_eq!(find(None, keychain(&items, &mut Vec::new())).unwrap(), Some(("ziggy", [2; 32])));
        // None at all: only then is a new key made.
        assert_eq!(find(None, keychain(&[], &mut Vec::new())).unwrap(), None);
        // A Keychain error stops the search rather than skipping to a key
        // that may not be this data's.
        let failing = |service: &'static str| if service == "vernam" { Err(anyhow::anyhow!("denied")) } else { Ok(Some([9; 32])) };
        assert!(find(None, failing).is_err());
    }
}
