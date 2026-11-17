//! Fakes for the tests here and the settings page's: an OS credential store, and an environment, that live in memory,
//! so no test touches a real keychain or this process's own environment.
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, bail};

use super::Host;
use crate::link::TokenStore;
use crate::sync::lock;

#[derive(Default)]
pub struct MemStore {
    entries: Mutex<BTreeMap<String, String>>,
    /// Every read fails, as a locked keychain's would.
    pub broken: Mutex<bool>,
}

impl MemStore {
    pub fn with(pairs: &[(&str, &str)]) -> Self {
        let store = Self::default();
        for (k, v) in pairs {
            lock(&store.entries).insert((*k).to_string(), (*v).to_string());
        }
        store
    }

    pub fn names(&self) -> Vec<String> {
        lock(&self.entries).keys().cloned().collect()
    }
}

impl TokenStore for MemStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        if *lock(&self.broken) {
            bail!("the store is locked");
        }
        Ok(lock(&self.entries).get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        lock(&self.entries).insert(account.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        lock(&self.entries).remove(account);
        Ok(())
    }
}

#[derive(Default)]
pub struct FakeHost {
    vars: Mutex<BTreeMap<String, String>>,
    git_logins: AtomicUsize,
}

impl FakeHost {
    pub fn with(pairs: &[(&str, &str)]) -> Self {
        let host = Self::default();
        for (k, v) in pairs {
            lock(&host.vars).insert((*k).to_string(), (*v).to_string());
        }
        host
    }

    pub fn git_logins(&self) -> usize {
        self.git_logins.load(Ordering::SeqCst)
    }

    pub fn all(&self) -> BTreeMap<String, String> {
        lock(&self.vars).clone()
    }
}

impl Host for FakeHost {
    fn get(&self, name: &str) -> Option<String> {
        lock(&self.vars).get(name).cloned().filter(|v| !v.is_empty())
    }
    fn set(&self, name: &str, value: &str) {
        lock(&self.vars).insert(name.to_string(), value.to_string());
    }
    fn remove(&self, name: &str) {
        lock(&self.vars).remove(name);
    }
    fn git_login(&self) {
        self.git_logins.fetch_add(1, Ordering::SeqCst);
    }
}
