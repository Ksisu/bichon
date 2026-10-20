pub mod account;
pub mod backup;
pub mod ext;
pub mod admin;
pub mod autoconfig;
pub mod archive;
pub mod common;
pub mod context;
pub mod dashboard;
pub mod database;
pub mod envelope;
pub mod error;
pub mod export;
pub mod imap;
pub mod import;
pub mod logger;
pub mod mailbox;
pub mod message;
pub mod migrate;
pub mod oauth2;
pub mod retention;
pub mod saved_search;
pub mod settings;
pub mod store;
pub mod system_config;
pub mod tasks;
pub mod token;
pub mod users;
pub mod utils;
pub mod version;

#[cfg(test)]
pub(crate) mod test_support {
    /// Serializes test code that **writes** the process-global envelope /
    /// attachment index.
    ///
    /// Test modules run in parallel and several of them share the same global
    /// `ENVELOPE_MANAGER` (tag updates, retention sweeps, ingest). Concurrent
    /// commits to one tantivy directory make Windows refuse the files with
    /// `PermissionDenied` ("拒绝访问"), which surfaces as rare failures in
    /// unrelated-looking tests. Any test that writes the global index must hold
    /// this lock for the duration of its writes.
    pub(crate) static GLOBAL_INDEX_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
}
