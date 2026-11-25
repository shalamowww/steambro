//! Changes that wait for Steam to restart. Steam has to be closed while its shortcuts change, so
//! the user may collect several changes and restart Steam once, when it suits them. A queued
//! change is the user's request for a game in one account; its artwork is already in the cache.
//! The next change that closes Steam, or finds it closed, applies the queue too.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::Request;

#[derive(Serialize, Deserialize, Default)]
pub struct Pending {
    pub changes: Vec<Queued>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Queued {
    /// The Steam account's ID.
    pub user: u32,
    pub key: String,
    pub request: Request,
}

impl Pending {
    /// A damaged file counts as an empty queue: the changes are only the user's wishes, and the
    /// window lets them choose again.
    pub fn load() -> Self {
        let Ok(path) = Self::path() else { return Self::default() };
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| crate::log!("{} is damaged, so no changes wait: {e}", path.display())).ok())
            .unwrap_or_default()
    }

    /// Writes a new file and then replaces the old one; an empty queue deletes the file.
    pub fn save(&self) -> crate::Result<()> {
        let path = Self::path()?;
        if self.changes.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().expect("pending path has a parent"))?;
        let temp = path.with_extension("json.new");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    /// Forgets what waits for these games in these accounts: the user's newer request replaces it.
    pub fn forget(&mut self, users: &[u32], keys: &[&str]) {
        self.changes.retain(|q| !(users.contains(&q.user) && keys.contains(&q.key.as_str())));
    }

    /// Queues a request, in place of one that waits for the same game in the same account.
    pub fn queue(&mut self, user: u32, key: &str, request: Request) {
        self.forget(&[user], &[key]);
        self.changes.push(Queued { user, key: key.to_string(), request });
    }

    /// The accounts with changes waiting, in the order they were queued.
    pub fn users(&self) -> Vec<u32> {
        let mut users = Vec::new();
        for q in &self.changes {
            if !users.contains(&q.user) {
                users.push(q.user);
            }
        }
        users
    }

    /// What waits in one account, as requests for `sync::prepare`.
    pub fn requests_for(&self, user: u32) -> Vec<(String, Request)> {
        self.changes.iter().filter(|q| q.user == user).map(|q| (q.key.clone(), q.request)).collect()
    }

    fn path() -> crate::Result<PathBuf> {
        Ok(crate::data_dir()?.join("pending.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_request_replaces_the_one_that_waits() {
        let mut pending = Pending::default();
        pending.queue(1, "xbox:a", Request::Add);
        pending.queue(2, "xbox:a", Request::Add);
        pending.queue(1, "battlenet:b", Request::Remove);
        pending.queue(1, "xbox:a", Request::Remove);
        assert_eq!(pending.requests_for(1), [("battlenet:b".to_string(), Request::Remove), ("xbox:a".to_string(), Request::Remove)]);
        assert_eq!(pending.requests_for(2), [("xbox:a".to_string(), Request::Add)]);
        assert_eq!(pending.users(), [2, 1]);
        pending.forget(&[1, 2], &["xbox:a"]);
        assert_eq!(pending.changes, [Queued { user: 1, key: "battlenet:b".into(), request: Request::Remove }]);
    }
}
