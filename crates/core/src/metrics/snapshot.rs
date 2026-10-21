//
// Copyright (c) 2025-2026 rustmailer.com (https://rustmailer.com)
//
// This file is part of the Bichon Email Archiving Project
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! State gauges computed per scrape.
//!
//! Collection ([`collect`]) and encoding ([`Snapshot::encode`]) are separate so
//! the encoding can be tested with hand-built snapshots, without a database or
//! an index.

use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    path::PathBuf,
    sync::{atomic::AtomicU64, Arc, LazyLock},
    time::{Duration, Instant},
};

use prometheus_client::{
    encoding::{text::encode_registry, EncodeLabelSet},
    metrics::{family::Family, gauge::Gauge},
    registry::{Registry, Unit},
};
use tracing::warn;

use super::LabelText;
use crate::{
    account::{
        entity::AuthType,
        migration::{AccountModel, AccountType},
        state::{DownloadState, DownloadStatus},
    },
    archive::imap::task::SYNC_TASKS,
    bichon_version,
    context::executors::BICHON_CONTEXT,
    error::BichonResult,
    settings::dir::DATA_DIR_MANAGER,
    store::tantivy::{attachment::ATTACHMENT_MANAGER, envelope::ENVELOPE_MANAGER},
    utils::get_total_size,
};

/// How long index document counts and on-disk sizes are reused between
/// scrapes. Keeps frequent (or HA-paired) Prometheus scrapes cheap.
const INDEX_TTL: Duration = Duration::from_secs(30);

static INDEX_CACHE: LazyLock<tokio::sync::Mutex<Option<(Instant, Arc<IndexSnapshot>)>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

label_values!(pub(super) enum AccountKind { Imap => "imap", NoSync => "nosync", View => "view" });
label_values!(pub(super) enum AuthKind { Password => "password", OAuth2 => "oauth2", None => "none" });
label_values!(enum Collector { Accounts => "accounts", Index => "index", Disk => "disk" });
label_values!(enum IndexName { Envelope => "envelope", Attachment => "attachment" });

/// Everything one scrape reports.
pub(super) struct Snapshot {
    pub start_time_seconds: f64,
    /// `None` when the accounts could not be read.
    pub accounts: Option<Vec<AccountRow>>,
    pub index: Arc<IndexSnapshot>,
}

pub(super) struct AccountRow {
    pub id: u64,
    pub email: String,
    pub name: String,
    pub kind: AccountKind,
    pub auth: AuthKind,
    pub enabled: bool,
    /// IMAP accounts that have synced at least once.
    pub sync: Option<SyncSnapshot>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SyncSnapshot {
    pub running: bool,
    pub last_finished_at_ms: Option<i64>,
    /// Whether the most recently finished session succeeded.
    pub last_success: Option<bool>,
    /// End of the newest successful session.
    pub last_success_end_ms: Option<i64>,
    /// Duration of the newest session that was not cancelled.
    pub last_duration_ms: Option<i64>,
}

/// Index document counts and on-disk sizes. A field is `None` when its source
/// could not be read.
#[derive(Debug, Default)]
pub(super) struct IndexSnapshot {
    pub emails: Option<u64>,
    pub attachments: Option<u64>,
    pub envelope_index_bytes: Option<u64>,
    pub attachment_index_bytes: Option<u64>,
    pub storage_bytes: Option<u64>,
}

impl IndexSnapshot {
    fn compute() -> Self {
        Self {
            emails: logged("count emails", ENVELOPE_MANAGER.total_emails(&None)),
            attachments: logged(
                "count attachments",
                ATTACHMENT_MANAGER.total_attachments(&None),
            ),
            envelope_index_bytes: dir_size(&DATA_DIR_MANAGER.envelope_dir),
            attachment_index_bytes: dir_size(&DATA_DIR_MANAGER.attachment_dir),
            storage_bytes: dir_size(&DATA_DIR_MANAGER.storage_dir),
        }
    }

    fn index_ok(&self) -> bool {
        self.emails.is_some() && self.attachments.is_some()
    }

    fn disk_ok(&self) -> bool {
        self.envelope_index_bytes.is_some()
            && self.attachment_index_bytes.is_some()
            && self.storage_bytes.is_some()
    }
}

fn logged<T, E: Debug>(what: &str, result: Result<T, E>) -> Option<T> {
    result
        .map_err(|e| warn!("Metrics: failed to {}: {:#?}", what, e))
        .ok()
}

/// A walk that fails (files Tantivy deletes mid-walk are already skipped, so
/// this is e.g. a permission error) leaves only that one series out.
fn dir_size(path: &PathBuf) -> Option<u64> {
    logged(&format!("measure {}", path.display()), get_total_size(path))
}

pub(super) async fn collect() -> Snapshot {
    let start_time_seconds = seconds(BICHON_CONTEXT.start_at());
    let busy = SYNC_TASKS.busy_account_ids().await;
    let accounts = match tokio::task::spawn_blocking(move || load_accounts(&busy)).await {
        Ok(Ok(rows)) => Some(rows),
        Ok(Err(e)) => {
            warn!("Metrics: failed to read accounts: {:#?}", e);
            None
        }
        Err(e) => {
            warn!("Metrics: account collection aborted: {:#?}", e);
            None
        }
    };
    Snapshot {
        start_time_seconds,
        accounts,
        index: index_snapshot().await,
    }
}

fn load_accounts(busy: &HashSet<u64>) -> BichonResult<Vec<AccountRow>> {
    let mut states: HashMap<u64, DownloadState> = DownloadState::list_all()?
        .into_iter()
        .map(|state| (state.account_id, state))
        .collect();
    Ok(AccountModel::list_all()?
        .into_iter()
        // An account being deleted is on its way out; reporting it would only
        // raise "never synced" alerts for something the user just removed.
        .filter(|account| !account.deleting)
        .map(|account| {
            let kind = match account.account_type {
                AccountType::IMAP => AccountKind::Imap,
                AccountType::NoSync => AccountKind::NoSync,
                AccountType::View => AccountKind::View,
            };
            let (auth, sync) = if kind == AccountKind::Imap {
                let auth = match account.imap.as_ref().map(|imap| &imap.auth.auth_type) {
                    Some(AuthType::Password) => AuthKind::Password,
                    Some(AuthType::OAuth2) => AuthKind::OAuth2,
                    None => AuthKind::None,
                };
                let sync = states
                    .remove(&account.id)
                    .map(|state| sync_snapshot(&state, busy.contains(&account.id)));
                (auth, sync)
            } else {
                (AuthKind::None, None)
            };
            AccountRow {
                id: account.id,
                email: account.email,
                name: account.account_name.unwrap_or_default(),
                kind,
                auth,
                enabled: account.enabled,
                sync,
            }
        })
        .collect())
}

/// Derives the sync gauges from an account's persisted download state. The
/// history holds the last finished sessions, newest last. `busy` is whether a
/// download task is working on the account in this process right now.
pub(super) fn sync_snapshot(state: &DownloadState, busy: bool) -> SyncSnapshot {
    let last = state.history.last();
    SyncSnapshot {
        // A persisted Running session outlives a crash (stale ones are only
        // finalized for enabled accounts), and the busy flag is also set while
        // a scheduler tick merely decides nothing is due, so require both.
        running: busy
            && state
                .active_session
                .as_ref()
                .is_some_and(|session| session.status == DownloadStatus::Running),
        // Timestamp and result both describe `last`, so they never mix two
        // sessions. The duration below may come from an older session.
        last_finished_at_ms: last.and_then(|session| session.end_time),
        last_success: last.map(|session| session.status == DownloadStatus::Success),
        last_success_end_ms: state.last_success_at.or_else(|| {
            // Records written before `last_success_at` existed.
            state
                .history
                .iter()
                .rev()
                .find(|session| session.status == DownloadStatus::Success)
                .and_then(|session| session.end_time)
        }),
        // A cancelled session's end time is when it was found interrupted,
        // possibly at the next startup, so its duration would include downtime.
        last_duration_ms: state
            .history
            .iter()
            .rev()
            .find(|session| session.status != DownloadStatus::Cancelled)
            .and_then(|session| {
                session
                    .end_time
                    .map(|end| (end - session.start_time).max(0))
            }),
    }
}

/// Reuses the last snapshot for [`INDEX_TTL`], including a partial one, so a
/// part that keeps failing is retried (and its warning logged) at most once
/// per TTL. The lock is held while computing, so concurrent scrapes share one
/// computation.
async fn index_snapshot() -> Arc<IndexSnapshot> {
    let mut cached = INDEX_CACHE.lock().await;
    if let Some((computed_at, snapshot)) = cached.as_ref() {
        if computed_at.elapsed() < INDEX_TTL {
            return Arc::clone(snapshot);
        }
    }
    let snapshot = match tokio::task::spawn_blocking(IndexSnapshot::compute).await {
        Ok(snapshot) => Arc::new(snapshot),
        Err(e) => {
            warn!("Metrics: index collection aborted: {:#?}", e);
            Arc::new(IndexSnapshot::default())
        }
    };
    *cached = Some((Instant::now(), Arc::clone(&snapshot)));
    snapshot
}

type SecondsGauge = Gauge<f64, AtomicU64>;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct BuildLabels {
    version: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct AccountInfoLabels {
    account_id: u64,
    email: LabelText,
    name: LabelText,
    r#type: AccountKind,
    auth: AuthKind,
    enabled: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct AccountLabel {
    account_id: u64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct IndexLabel {
    index: IndexName,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CollectorLabel {
    collector: Collector,
}

fn seconds(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

fn gauge_value(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Snapshot {
    /// Writes every series of this snapshot (without the `# EOF` marker).
    pub(super) fn encode(&self, out: &mut String) -> std::fmt::Result {
        let mut registry = Registry::with_prefix("bichon");

        let build_info = Family::<BuildLabels, Gauge>::default();
        build_info
            .get_or_create(&BuildLabels {
                version: bichon_version!(),
            })
            .set(1);
        registry.register(
            "build_info",
            "Bichon build information, the value is always 1",
            build_info,
        );

        let start_time = SecondsGauge::default();
        start_time.set(self.start_time_seconds);
        registry.register_with_unit(
            "start_time",
            "Unix time at which the server process started",
            Unit::Seconds,
            start_time,
        );

        self.encode_accounts(&mut registry);
        self.encode_index(&mut registry);

        let collector_success = Family::<CollectorLabel, Gauge>::default();
        for (collector, ok) in [
            (Collector::Accounts, self.accounts.is_some()),
            (Collector::Index, self.index.index_ok()),
            (Collector::Disk, self.index.disk_ok()),
        ] {
            collector_success
                .get_or_create(&CollectorLabel { collector })
                .set(i64::from(ok));
        }
        registry.register(
            "scrape_collector_success",
            "Whether a part of this scrape could be collected (1) or was left out (0)",
            collector_success,
        );

        encode_registry(out, &registry)
    }

    fn encode_accounts(&self, registry: &mut Registry) {
        let Some(accounts) = &self.accounts else {
            return;
        };
        let info = Family::<AccountInfoLabels, Gauge>::default();
        let running = Family::<AccountLabel, Gauge>::default();
        let last_sync = Family::<AccountLabel, SecondsGauge>::default();
        let last_success = Family::<AccountLabel, Gauge>::default();
        let last_successful_sync = Family::<AccountLabel, SecondsGauge>::default();
        let last_duration = Family::<AccountLabel, SecondsGauge>::default();

        for account in accounts {
            info.get_or_create(&AccountInfoLabels {
                account_id: account.id,
                email: LabelText(account.email.clone()),
                name: LabelText(account.name.clone()),
                r#type: account.kind,
                auth: account.auth,
                enabled: account.enabled,
            })
            .set(1);

            let Some(sync) = &account.sync else {
                continue;
            };
            let label = AccountLabel {
                account_id: account.id,
            };
            running.get_or_create(&label).set(i64::from(sync.running));
            if let Some(at) = sync.last_finished_at_ms {
                last_sync.get_or_create(&label).set(seconds(at));
            }
            if let Some(ok) = sync.last_success {
                last_success.get_or_create(&label).set(i64::from(ok));
            }
            if let Some(at) = sync.last_success_end_ms {
                last_successful_sync.get_or_create(&label).set(seconds(at));
            }
            if let Some(ms) = sync.last_duration_ms {
                last_duration.get_or_create(&label).set(seconds(ms));
            }
        }

        registry.register(
            "account_info",
            "Account metadata for joins on account_id, the value is always 1",
            info,
        );
        registry.register(
            "account_sync_running",
            "Whether a download session is currently running for the account",
            running,
        );
        registry.register_with_unit(
            "account_last_sync_timestamp",
            "Unix time the last download session finished, whatever its result",
            Unit::Seconds,
            last_sync,
        );
        registry.register(
            "account_last_sync_success",
            "Whether the last finished download session succeeded (failed or cancelled is 0)",
            last_success,
        );
        registry.register_with_unit(
            "account_last_successful_sync_timestamp",
            "Unix time the newest successful download session finished",
            Unit::Seconds,
            last_successful_sync,
        );
        registry.register_with_unit(
            "account_last_sync_duration",
            "Duration of the last download session that succeeded or failed (cancelled sessions are skipped)",
            Unit::Seconds,
            last_duration,
        );
    }

    fn encode_index(&self, registry: &mut Registry) {
        let index = &self.index;
        if let Some(emails) = index.emails {
            let gauge = Gauge::<i64>::default();
            gauge.set(gauge_value(emails));
            registry.register("emails", "Emails in the archive", gauge);
        }
        if let Some(attachments) = index.attachments {
            let gauge = Gauge::<i64>::default();
            gauge.set(gauge_value(attachments));
            registry.register("attachments", "Attachments in the archive", gauge);
        }

        let index_size = Family::<IndexLabel, Gauge>::default();
        for (name, bytes) in [
            (IndexName::Envelope, index.envelope_index_bytes),
            (IndexName::Attachment, index.attachment_index_bytes),
        ] {
            if let Some(bytes) = bytes {
                index_size
                    .get_or_create(&IndexLabel { index: name })
                    .set(gauge_value(bytes));
            }
        }
        registry.register_with_unit(
            "index_size",
            "On-disk size of a full-text index",
            Unit::Bytes,
            index_size,
        );

        if let Some(bytes) = index.storage_bytes {
            let gauge = Gauge::<i64>::default();
            gauge.set(gauge_value(bytes));
            registry.register_with_unit(
                "storage_size",
                "On-disk size of the message and attachment blob storage",
                Unit::Bytes,
                gauge,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use prometheus_client::encoding::text::encode_eof;

    use super::*;
    use crate::account::state::DownloadSession;

    fn render(snapshot: &Snapshot) -> String {
        let mut out = String::new();
        snapshot.encode(&mut out).unwrap();
        encode_eof(&mut out).unwrap();
        out
    }

    fn full_index() -> Arc<IndexSnapshot> {
        Arc::new(IndexSnapshot {
            emails: Some(42),
            attachments: Some(7),
            envelope_index_bytes: Some(1000),
            attachment_index_bytes: Some(200),
            storage_bytes: Some(3000),
        })
    }

    fn imap_row(id: u64, sync: Option<SyncSnapshot>) -> AccountRow {
        AccountRow {
            id,
            email: format!("user{id}@example.com"),
            name: "Personal".into(),
            kind: AccountKind::Imap,
            auth: AuthKind::OAuth2,
            enabled: true,
            sync,
        }
    }

    fn assert_unique_types(out: &str) {
        let mut names: Vec<&str> = out
            .lines()
            .filter_map(|line| line.strip_prefix("# TYPE "))
            .filter_map(|rest| rest.split(' ').next())
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(total, names.len(), "duplicate # TYPE lines:\n{out}");
    }

    #[test]
    fn encodes_accounts_sync_state_and_totals() {
        let snapshot = Snapshot {
            start_time_seconds: 1_700_000_000.5,
            accounts: Some(vec![
                imap_row(
                    7,
                    Some(SyncSnapshot {
                        running: false,
                        last_finished_at_ms: Some(1_700_000_100_000),
                        last_success: Some(true),
                        last_success_end_ms: Some(1_700_000_100_000),
                        last_duration_ms: Some(2_500),
                    }),
                ),
                AccountRow {
                    id: 8,
                    email: "inbox@example.com".into(),
                    name: String::new(),
                    kind: AccountKind::NoSync,
                    auth: AuthKind::None,
                    enabled: true,
                    sync: None,
                },
            ]),
            index: full_index(),
        };
        let out = render(&snapshot);

        for expected in [
            "# TYPE bichon_build_info gauge",
            &format!("bichon_build_info{{version=\"{}\"}} 1", bichon_version!()),
            "# TYPE bichon_start_time_seconds gauge",
            "# UNIT bichon_start_time_seconds seconds",
            "# TYPE bichon_account_info gauge",
            "bichon_account_info{account_id=\"7\",email=\"user7@example.com\",name=\"Personal\",type=\"imap\",auth=\"oauth2\",enabled=\"true\"} 1",
            "bichon_account_info{account_id=\"8\",email=\"inbox@example.com\",name=\"\",type=\"nosync\",auth=\"none\",enabled=\"true\"} 1",
            "bichon_account_sync_running{account_id=\"7\"} 0",
            "bichon_account_last_sync_timestamp_seconds{account_id=\"7\"} 1700000100.0",
            "bichon_account_last_sync_success{account_id=\"7\"} 1",
            "bichon_account_last_successful_sync_timestamp_seconds{account_id=\"7\"} 1700000100.0",
            "bichon_account_last_sync_duration_seconds{account_id=\"7\"} 2.5",
            "bichon_emails 42",
            "bichon_attachments 7",
            "bichon_index_size_bytes{index=\"envelope\"} 1000",
            "bichon_index_size_bytes{index=\"attachment\"} 200",
            "bichon_storage_size_bytes 3000",
            "bichon_scrape_collector_success{collector=\"accounts\"} 1",
            "bichon_scrape_collector_success{collector=\"index\"} 1",
            "bichon_scrape_collector_success{collector=\"disk\"} 1",
        ] {
            assert!(out.contains(expected), "missing `{expected}` in:\n{out}");
        }
        assert!(!out.contains("r#type"), "raw identifier leaked:\n{out}");
        assert!(
            !out.contains("account_sync_running{account_id=\"8\"}"),
            "a NoSync account has no sync gauges:\n{out}"
        );
        assert!(out.ends_with("# EOF\n"));
        assert_unique_types(&out);
    }

    #[test]
    fn failed_parts_are_reported_and_left_out() {
        let snapshot = Snapshot {
            start_time_seconds: 0.0,
            accounts: None,
            index: Arc::new(IndexSnapshot {
                emails: Some(1),
                attachments: None,
                envelope_index_bytes: Some(1),
                attachment_index_bytes: Some(1),
                storage_bytes: None,
            }),
        };
        let out = render(&snapshot);
        assert!(out.contains("bichon_scrape_collector_success{collector=\"accounts\"} 0"));
        assert!(out.contains("bichon_scrape_collector_success{collector=\"index\"} 0"));
        assert!(out.contains("bichon_scrape_collector_success{collector=\"disk\"} 0"));
        assert!(out.contains("bichon_emails 1"));
        assert!(!out.contains("bichon_account_info"));
        assert!(!out.contains("bichon_attachments"));
        assert!(!out.contains("bichon_storage_size_bytes"));
        assert!(out.ends_with("# EOF\n"));
        assert_unique_types(&out);
    }

    #[test]
    fn account_names_are_escaped() {
        let mut row = imap_row(9, None);
        row.name = "A \"quoted\" \\ name".into();
        let snapshot = Snapshot {
            start_time_seconds: 0.0,
            accounts: Some(vec![row]),
            index: full_index(),
        };
        let out = render(&snapshot);
        assert!(
            out.contains(r#"name="A \"quoted\" \\ name""#),
            "name not escaped:\n{out}"
        );
    }

    fn session(status: DownloadStatus, start: i64, end: Option<i64>) -> DownloadSession {
        DownloadSession {
            start_time: start,
            end_time: end,
            status,
            ..Default::default()
        }
    }

    #[test]
    fn sync_snapshot_reports_the_latest_session_and_newest_success() {
        let state = DownloadState {
            account_id: 1,
            active_session: Some(session(DownloadStatus::Running, 5_000, None)),
            history: vec![
                session(DownloadStatus::Success, 1_000, Some(1_500)),
                session(DownloadStatus::Failed, 2_000, Some(4_000)),
            ],
            last_trigger_at: 5_000,
            last_finished_at: Some(4_000),
            // Written before the field existed: falls back to the history.
            last_success_at: None,
        };
        assert_eq!(
            sync_snapshot(&state, true),
            SyncSnapshot {
                running: true,
                last_finished_at_ms: Some(4_000),
                last_success: Some(false),
                last_success_end_ms: Some(1_500),
                last_duration_ms: Some(2_000),
            }
        );
    }

    #[test]
    fn sync_snapshot_prefers_the_persisted_last_success() {
        let state = DownloadState {
            account_id: 1,
            history: vec![session(DownloadStatus::Failed, 2_000, Some(4_000))],
            last_finished_at: Some(4_000),
            last_success_at: Some(1_500),
            ..Default::default()
        };
        assert_eq!(
            sync_snapshot(&state, false).last_success_end_ms,
            Some(1_500)
        );
    }

    /// `start_new_session` moves an interrupted session into the history as
    /// Cancelled without touching `last_finished_at`, and a session finalized
    /// at startup ends at the restart, hours after it really stopped.
    #[test]
    fn sync_snapshot_after_an_interrupted_session() {
        let state = DownloadState {
            account_id: 1,
            history: vec![
                session(DownloadStatus::Success, 1_000, Some(1_500)),
                session(DownloadStatus::Cancelled, 2_000, Some(30_000)),
            ],
            last_finished_at: Some(1_500),
            last_success_at: Some(1_500),
            ..Default::default()
        };
        assert_eq!(
            sync_snapshot(&state, false),
            SyncSnapshot {
                running: false,
                // Same session as `last_success`, not the stale field.
                last_finished_at_ms: Some(30_000),
                last_success: Some(false),
                last_success_end_ms: Some(1_500),
                // The cancelled session's 28s are not a sync duration.
                last_duration_ms: Some(500),
            }
        );
    }

    #[test]
    fn sync_snapshot_running_needs_a_live_task() {
        let stale = DownloadState {
            account_id: 1,
            active_session: Some(session(DownloadStatus::Running, 5_000, None)),
            ..Default::default()
        };
        assert!(
            !sync_snapshot(&stale, false).running,
            "a Running session left over from a crash is not running"
        );
        assert!(
            !sync_snapshot(&DownloadState::empty(1), true).running,
            "a busy tick that started no session is not running"
        );
    }

    #[test]
    fn sync_snapshot_of_a_fresh_account_is_empty() {
        let state = DownloadState::empty(1);
        assert_eq!(sync_snapshot(&state, false), SyncSnapshot::default());
    }
}
