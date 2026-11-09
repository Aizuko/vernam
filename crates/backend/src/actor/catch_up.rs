//! Draining the server's queue after connecting, and where the time goes.
//!
//! Signal Desktop holds work back while the queue drains and does it once
//! the server says it's empty (`pauseQueuesAndNotificationsOnSocketConnect`
//! and `restartQueuesAndNotificationsOnEmpty` in `ts/background.preload.ts`).
//! Here that means:
//!
//! - A contact sync's renaming is done once, at the end. Each one re-titles
//!   every chat and renames every author, and the phone can send several: a
//!   two-day backlog had four, each holding up receiving for 3 to 4 seconds.
//!   presage has saved the contacts by then, and the app hears at once that
//!   they came. The pending renaming is kept in a setting, so quitting
//!   before the queue empties doesn't lose it.
//! - The chat list reloads at most every couple of seconds rather than after
//!   each message, as its query reads every chat.
//! - Downloads that start on their own run a few at a time
//!   (`MAX_CONCURRENT_JOBS` in `ts/jobs/AttachmentDownloadManager.preload.ts`),
//!   whether or not the queue is draining.
//!
//! Each envelope's time is split into waiting on presage's stream (decrypting
//! and presage's own storing) and our handling of it, which is broken into
//! phases. A slow envelope is logged with its phases, and the drain ends with
//! a summary.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::*;

/// An envelope slower than this is logged with where its time went.
const SLOW_ENVELOPE: Duration = Duration::from_millis(500);
/// While the queue drains, the chat list reloads at most this often.
const LIST_REFRESH: Duration = Duration::from_secs(2);
/// Desktop's `MAX_CONCURRENT_JOBS` for attachment downloads.
const AUTO_DOWNLOADS: usize = 6;
/// Phases named in a slow envelope's line, and in the summary.
const PHASES_SHOWN: usize = 6;
const SUMMARY_PHASES_SHOWN: usize = 12;
/// A contact sync's renaming waits for the queue to empty.
const SETTING_CONTACTS_PENDING: &str = "catch_up.contacts_pending";

pub(super) struct Draining {
    /// From connecting until the server's queue is empty.
    active: Cell<bool>,
    stats: RefCell<Stats>,
    /// A chat list change waits for the next refresh, which is scheduled.
    list_held: Rc<Cell<bool>>,
    list_scheduled: Rc<Cell<bool>>,
    events: mpsc::UnboundedSender<Event>,
    /// Permits for downloads that start on their own; replaced when the
    /// account is wiped, so those still waiting don't run for the next one.
    downloads: RefCell<Rc<tokio::sync::Semaphore>>,
}

#[derive(Default)]
struct Stats {
    started: Option<Instant>,
    envelopes: u32,
    stream: Duration,
    handling: Duration,
    slowest: Duration,
    /// Totals by phase over the drain: count and time.
    phases: HashMap<&'static str, (u32, Duration)>,
    /// The envelope being handled, while one is.
    current: Option<Vec<(&'static str, Duration)>>,
}

/// Times a phase of handling an envelope, until dropped.
pub(super) struct Phase<'a> {
    stats: &'a RefCell<Stats>,
    name: &'static str,
    started: Instant,
}

impl Drop for Phase<'_> {
    fn drop(&mut self) {
        // (Only the receive loop's: `apply` also runs for other work. Work
        // interleaved with an envelope's can still land on it.)
        if let Some(current) = &mut self.stats.borrow_mut().current {
            current.push((self.name, self.started.elapsed()));
        }
    }
}

/// Ends the drain when the receive loop stops, however it stops (an
/// aborted task included).
pub(super) struct DrainGuard(Rc<Actor>);

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.0.drain_finished(false);
    }
}

impl Draining {
    pub(super) fn new(events: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            active: Cell::new(false),
            stats: RefCell::default(),
            list_held: Rc::default(),
            list_scheduled: Rc::default(),
            events,
            downloads: RefCell::new(Rc::new(tokio::sync::Semaphore::new(AUTO_DOWNLOADS))),
        }
    }

    fn phase(&self, name: &'static str) -> Phase<'_> {
        Phase { stats: &self.stats, name, started: Instant::now() }
    }

    fn start(&self) {
        self.active.set(true);
        *self.stats.borrow_mut() = Stats { started: Some(Instant::now()), ..Default::default() };
    }

    /// Ends the drain: its summary, or `None` if none was running. A held
    /// chat list change goes out.
    fn finish(&self) -> Option<Summary> {
        // An envelope cut off (the loop aborted) leaves no phases collecting.
        self.stats.borrow_mut().current = None;
        if !self.active.replace(false) {
            return None;
        }
        if self.list_held.replace(false) {
            let _ = self.events.unbounded_send(Event::ConversationsChanged);
        }
        let stats = std::mem::take(&mut *self.stats.borrow_mut());
        let mut phases: Vec<_> = stats.phases.into_iter().collect();
        phases.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(b.0)));
        let phases = phases
            .iter()
            .take(SUMMARY_PHASES_SHOWN)
            .map(|(name, (count, time))| format!("{name}={}ms/{count}", time.as_millis()))
            .collect::<Vec<_>>()
            .join(" ");
        Some(Summary {
            envelopes: stats.envelopes,
            elapsed: stats.started.map_or(Duration::ZERO, |s| s.elapsed()),
            stream: stats.stream,
            handling: stats.handling,
            slowest: stats.slowest,
            phases,
        })
    }

    fn envelope_started(&self) {
        self.stats.borrow_mut().current = Some(Vec::new());
    }

    /// Counts an envelope; its phases, slowest first, if it was slow. Outside
    /// a drain, waiting on the stream is mostly waiting for someone to write,
    /// so only our handling counts there.
    fn envelope_done(&self, stream: Duration, handling: Duration) -> Option<String> {
        let mut stats = self.stats.borrow_mut();
        let current = stats.current.take().unwrap_or_default();
        let total = stream + handling;
        let active = self.active.get();
        let slow = (if active { total } else { handling }) >= SLOW_ENVELOPE;
        let slow = slow.then(|| {
            let mut shown = current.clone();
            shown.sort_by(|a, b| b.1.cmp(&a.1));
            shown
                .iter()
                .take(PHASES_SHOWN)
                .map(|(name, time)| format!("{name}={}ms", time.as_millis()))
                .collect::<Vec<_>>()
                .join(" ")
        });
        if active {
            stats.envelopes += 1;
            stats.stream += stream;
            stats.handling += handling;
            stats.slowest = stats.slowest.max(total);
            for (name, time) in current {
                let entry = stats.phases.entry(name).or_default();
                entry.0 += 1;
                entry.1 += time;
            }
        }
        slow
    }

    /// Whether to hold this event back. The first held schedules the
    /// refresh, which sends it unless the drain's end already has.
    fn hold(&self, event: &Event) -> bool {
        if !self.active.get() || !matches!(event, Event::ConversationsChanged) {
            return false;
        }
        self.list_held.set(true);
        if !self.list_scheduled.replace(true) {
            let (held, scheduled, events) = (self.list_held.clone(), self.list_scheduled.clone(), self.events.clone());
            tokio::task::spawn_local(async move {
                tokio::time::sleep(LIST_REFRESH).await;
                scheduled.set(false);
                if held.replace(false) {
                    let _ = events.unbounded_send(Event::ConversationsChanged);
                }
            });
        }
        true
    }

    fn spawn_download(&self, download: impl Future<Output = ()> + 'static) {
        let permits = self.downloads.borrow().clone();
        tokio::task::spawn_local(async move {
            // Closed: the account was wiped.
            let Ok(_permit) = permits.acquire().await else { return };
            download.await;
        });
    }

    /// Drops the downloads still waiting for a slot.
    fn drop_waiting_downloads(&self) {
        let fresh = Rc::new(tokio::sync::Semaphore::new(AUTO_DOWNLOADS));
        self.downloads.replace(fresh).close();
    }
}

/// What a queued download is for.
pub(super) enum File {
    Attachment(usize),
    Preview,
    Story,
}

struct Summary {
    envelopes: u32,
    elapsed: Duration,
    stream: Duration,
    handling: Duration,
    slowest: Duration,
    phases: String,
}

impl Actor {
    /// Times `name` until the guard drops. Phases may nest (`insert` and
    /// `insert.store`); the summary reports each on its own, so its totals
    /// add up to more than the time taken.
    pub(super) fn phase(&self, name: &'static str) -> Phase<'_> {
        self.draining.phase(name)
    }

    /// The receive loop connected: the queue drains until the guard drops
    /// or the server says it's empty.
    pub(super) fn drain_started(self: &Rc<Self>) -> DrainGuard {
        self.draining.start();
        DrainGuard(self.clone())
    }

    /// The queue is empty (`emptied`) or the connection ended first.
    pub(super) fn drain_finished(&self, emptied: bool) {
        let Some(summary) = self.draining.finish() else { return };
        info!(
            emptied,
            envelopes = summary.envelopes,
            elapsed_ms = summary.elapsed.as_millis(),
            stream_ms = summary.stream.as_millis(),
            handling_ms = summary.handling.as_millis(),
            slowest_ms = summary.slowest.as_millis(),
            phases = %summary.phases,
            "queue drained"
        );
    }

    /// The receive loop starts handling an envelope.
    pub(super) fn envelope_started(&self) {
        self.draining.envelope_started();
    }

    /// One envelope done: `stream` waiting on presage for it, `handling` ours.
    pub(super) fn envelope_done(&self, kind: &'static str, stream: Duration, handling: Duration) {
        if let Some(phases) = self.draining.envelope_done(stream, handling) {
            warn!(kind, stream_ms = stream.as_millis(), handling_ms = handling.as_millis(), %phases, "slow envelope");
        }
    }

    /// Whether `emit` holds this event back while draining.
    pub(super) fn holds(&self, event: &Event) -> bool {
        self.draining.hold(event)
    }

    /// Contacts came from the phone (presage has saved them). The app hears
    /// at once; renaming waits while the queue drains.
    pub(super) async fn on_contacts(self: &Rc<Self>) {
        let _ = self.db.set_setting(crate::SETTING_CONTACTS_SYNCED_AT, &now_millis().to_string()).await;
        self.emit(Event::ContactsSynced);
        if self.draining.active.get() {
            // Names looked up from here on come from the new contacts (a
            // mention's name is written into its message for good).
            self.names.borrow_mut().clear();
            self.titled.borrow_mut().clear();
            match self.db.set_setting(SETTING_CONTACTS_PENDING, "1").await {
                Ok(()) => return info!("contacts synchronized; renaming once the queue is empty"),
                Err(error) => warn!(%error, "couldn't hold the contact sync; renaming now"),
            }
        }
        self.contacts_synced().await;
    }

    /// A contact sync's renaming held while draining (this run or an earlier
    /// one), now done; `false` if none was.
    pub(super) async fn apply_held_contacts(self: &Rc<Self>) -> bool {
        match self.db.setting(SETTING_CONTACTS_PENDING).await {
            Ok(Some(_)) => {}
            Ok(None) => return false,
            Err(error) => {
                warn!(%error, "couldn't read whether a contact sync is held; trying when the queue next empties");
                return false;
            }
        }
        self.contacts_synced().await;
        true
    }

    /// Names and titles follow the contacts in presage's store, which
    /// settles any renaming held for them.
    pub(super) async fn contacts_synced(self: &Rc<Self>) {
        info!("applying synchronized contacts");
        self.names.borrow_mut().clear();
        self.titled.borrow_mut().clear();
        let titled = self.refresh_titles().await;
        let renamed = self.backfill_author_names().await;
        self.save_contact_avatars().await;
        // Only once it's done: a quit or a failure meanwhile leaves it for
        // the next time the queue empties.
        if !(titled && renamed) {
            warn!("contacts not fully applied; trying again when the queue next empties");
            // (Also when it wasn't held.)
            if let Err(error) = self.db.set_setting(SETTING_CONTACTS_PENDING, "1").await {
                warn!(%error, "couldn't keep the contact sync for later");
            }
            return;
        }
        if let Err(error) = self.db.remove_setting(SETTING_CONTACTS_PENDING).await {
            warn!(%error, "couldn't clear the held contact sync");
        }
    }

    /// Runs a download that starts on its own once one of the few slots is
    /// free.
    pub(super) fn spawn_auto_download(&self, download: impl Future<Output = ()> + 'static) {
        self.draining.spawn_download(download);
    }

    /// Whether a queued download's file needn't come: it came meanwhile
    /// (opened first, which fetches it at once, and the download would write
    /// it again under whoever is viewing it), or the row is gone. A row whose
    /// time isn't `sent_at` is another one that took a deleted row's id.
    /// A failed read counts as there, as the next open fetches it.
    pub(super) async fn has_file(&self, id: i64, sent_at: i64, file: File) -> bool {
        let there = match file {
            File::Attachment(index) => self.db.message(id).await.map(|m| {
                m.filter(|m| m.sent_at == sent_at)
                    .is_none_or(|m| m.attachments.get(index).is_none_or(|a| a.local_path.is_some()))
            }),
            File::Preview => self.db.message(id).await.map(|m| {
                m.filter(|m| m.sent_at == sent_at)
                    .is_none_or(|m| m.link_preview.and_then(|p| p.image).is_none_or(|i| i.local_path.is_some()))
            }),
            File::Story => self.db.story(id).await.map(|s| {
                s.filter(|s| s.sent_at == sent_at).is_none_or(|s| {
                    !matches!(s.content, signal_store::StoryContent::Media { attachment, .. } if attachment.local_path.is_none())
                })
            }),
        };
        there.unwrap_or_else(|error| {
            warn!(%error, id, "couldn't check a queued download; skipping it (opening it fetches it)");
            true
        })
    }

    /// The account is wiped: downloads still waiting don't run.
    pub(super) fn drop_waiting_downloads(&self) {
        self.draining.drop_waiting_downloads();
    }
}

/// What an envelope was, for the log.
pub(super) fn envelope_kind(received: &Received) -> &'static str {
    use sync_message::Content as Sync;
    let content = match received {
        Received::QueueEmpty => return "queue empty",
        Received::Contacts => return "contacts",
        Received::DecryptionError(..) => return "decryption error",
        Received::RetryRequest(_) => return "retry request",
        Received::Content(content) => content,
    };
    match &content.body {
        ContentBody::NullMessage(_) => "null",
        ContentBody::DataMessage(dm) if dm.group_v2.is_some() => "group message",
        ContentBody::DataMessage(_) => "message",
        ContentBody::CallMessage(_) => "call",
        ContentBody::ReceiptMessage(_) => "receipt",
        ContentBody::TypingMessage(_) => "typing",
        ContentBody::DecryptionErrorMessage(_) => "decryption error message",
        ContentBody::StoryMessage(_) => "story",
        ContentBody::EditMessage(_) => "edit",
        ContentBody::SynchronizeMessage(sync) => match &sync.content {
            Some(Sync::Sent(sent)) if sent.story_message.is_some() => "sync sent story",
            Some(Sync::Sent(sent)) if sent.edit_message.is_some() => "sync sent edit",
            Some(Sync::Sent(_)) => "sync sent",
            Some(Sync::Contacts(_)) => "sync contacts",
            Some(Sync::Request(_)) => "sync request",
            Some(Sync::Blocked(_)) => "sync blocked",
            Some(Sync::Verified(_)) => "sync verified",
            Some(Sync::Configuration(_)) => "sync configuration",
            Some(Sync::ViewOnceOpen(_)) => "sync view once open",
            Some(Sync::FetchLatest(_)) => "sync fetch latest",
            Some(Sync::Keys(_)) => "sync keys",
            Some(Sync::MessageRequestResponse(_)) => "sync message request response",
            Some(Sync::CallEvent(_)) => "sync call event",
            Some(Sync::CallLinkUpdate(_)) => "sync call link update",
            Some(Sync::CallLogEvent(_)) => "sync call log event",
            Some(Sync::DeleteForMe(_)) => "sync delete for me",
            Some(_) => "sync other",
            None if !sync.read.is_empty() => "sync read",
            None if !sync.viewed.is_empty() => "sync viewed",
            None if !sync.sticker_pack_operation.is_empty() => "sync sticker packs",
            None => "sync empty",
        },
    }
}

/// An action's phase name.
pub(super) fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Insert { .. } => "insert",
        Action::React { .. } => "react",
        Action::Delete { .. } | Action::AdminDelete { .. } | Action::DeleteForMe { .. } => "delete",
        Action::Edit { .. } => "edit",
        Action::Receipt { .. } => "receipt",
        Action::Typing { .. } => "typing",
        Action::ReadSync { .. } | Action::ViewedSync { .. } | Action::ViewOnceOpen { .. } => "read sync",
        Action::Story { .. } | Action::StoryReply { .. } | Action::StoryViewed { .. } | Action::StoryRecipientUpdate { .. } => "story",
        Action::CallEvent { .. }
        | Action::GroupCallEvent { .. }
        | Action::CallLogEvent { .. }
        | Action::CallLinkUpdate { .. }
        | Action::AdhocCallEvent { .. }
        | Action::GroupCallStarted { .. } => "call event",
        Action::StorageChanged { .. } | Action::LocalProfileChanged { .. } | Action::AccountSettings { .. } => "account sync",
        Action::PollVote { .. } | Action::PollEnd { .. } => "poll",
        Action::Notice { .. } => "notice",
        _ => "other action",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draining() -> (Draining, mpsc::UnboundedReceiver<Event>) {
        let (events, received) = mpsc::unbounded();
        (Draining::new(events), received)
    }

    fn sent(received: &mut mpsc::UnboundedReceiver<Event>) -> usize {
        std::iter::from_fn(|| received.try_recv().ok()).count()
    }

    #[tokio::test(start_paused = true)]
    async fn the_chat_list_waits_while_draining() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (draining, mut received) = draining();
                assert!(!draining.hold(&Event::ConversationsChanged), "not draining: it goes out");
                draining.start();
                assert!(!draining.hold(&Event::ContactsSynced), "only the chat list waits");
                assert!(draining.hold(&Event::ConversationsChanged));
                assert!(draining.hold(&Event::ConversationsChanged));
                tokio::task::yield_now().await;
                assert_eq!(sent(&mut received), 0);
                tokio::time::sleep(LIST_REFRESH).await;
                tokio::task::yield_now().await;
                assert_eq!(sent(&mut received), 1, "one refresh for both");

                // Held again, then the drain ends first: it goes out then,
                // and the scheduled refresh has nothing left to send.
                assert!(draining.hold(&Event::ConversationsChanged));
                draining.finish();
                assert_eq!(sent(&mut received), 1);
                tokio::time::sleep(LIST_REFRESH * 2).await;
                assert_eq!(sent(&mut received), 0);
                assert!(!draining.hold(&Event::ConversationsChanged), "drain over");
            })
            .await;
    }

    #[test]
    fn the_summary_counts_envelopes_and_phases_of_the_drain() {
        let (draining, _received) = draining();
        assert!(draining.finish().is_none(), "no drain running");
        // Before the drain: not counted, and waiting on the stream (for
        // someone to write) isn't slow.
        draining.envelope_started();
        drop(draining.phase("insert"));
        assert!(draining.envelope_done(Duration::from_secs(60), Duration::from_millis(1)).is_none());
        draining.envelope_started();
        assert!(draining.envelope_done(Duration::ZERO, SLOW_ENVELOPE).is_some(), "but handling can be");

        draining.start();
        // Outside an envelope, a phase isn't kept.
        drop(draining.phase("elsewhere"));
        for _ in 0..2 {
            draining.envelope_started();
            drop(draining.phase("insert"));
            drop(draining.phase("identity"));
            assert!(draining.envelope_done(Duration::from_millis(3), Duration::from_millis(4)).is_none());
        }
        draining.envelope_started();
        let slow = draining.envelope_done(SLOW_ENVELOPE, Duration::from_millis(100));
        assert_eq!(slow.as_deref(), Some(""), "slow while draining, with no phases timed");

        let summary = draining.finish().unwrap();
        assert_eq!(summary.envelopes, 3);
        assert_eq!(summary.stream, Duration::from_millis(6) + SLOW_ENVELOPE);
        assert_eq!(summary.handling, Duration::from_millis(108));
        assert_eq!(summary.slowest, Duration::from_millis(100) + SLOW_ENVELOPE);
        assert!(summary.phases.contains("insert=") && summary.phases.contains("ms/2"), "{}", summary.phases);
        assert!(!summary.phases.contains("elsewhere"), "{}", summary.phases);
        assert!(draining.finish().is_none(), "finished once");

        // A loop aborted mid-envelope: nothing collects phases afterwards.
        draining.envelope_started();
        assert!(draining.finish().is_none());
        drop(draining.phase("later"));
        assert!(draining.stats.borrow().current.is_none());
    }

    #[tokio::test]
    async fn downloads_run_a_few_at_a_time() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (draining, _received) = draining();
                let running = Rc::new(Cell::new(0));
                let most = Rc::new(Cell::new(0));
                let (done, mut finished) = mpsc::unbounded::<()>();
                for _ in 0..AUTO_DOWNLOADS * 3 {
                    let (running, most, done) = (running.clone(), most.clone(), done.clone());
                    draining.spawn_download(async move {
                        running.set(running.get() + 1);
                        most.set(most.get().max(running.get()));
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        running.set(running.get() - 1);
                        let _ = done.unbounded_send(());
                    });
                }
                for _ in 0..AUTO_DOWNLOADS * 3 {
                    finished.next().await.unwrap();
                }
                assert_eq!(most.get(), AUTO_DOWNLOADS);
            })
            .await;
    }

    #[tokio::test]
    async fn downloads_waiting_when_the_account_is_wiped_dont_run() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (draining, _received) = draining();
                let ran = Rc::new(Cell::new(0));
                let (release, wait) = tokio::sync::watch::channel(false);
                for _ in 0..AUTO_DOWNLOADS + 2 {
                    let (ran, mut wait) = (ran.clone(), wait.clone());
                    draining.spawn_download(async move {
                        ran.set(ran.get() + 1);
                        let _ = wait.wait_for(|go| *go).await;
                    });
                }
                tokio::task::yield_now().await;
                assert_eq!(ran.get(), AUTO_DOWNLOADS);
                draining.drop_waiting_downloads();
                release.send(true).unwrap();
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(ran.get(), AUTO_DOWNLOADS, "the two waiting never started");
                // New ones run.
                let after = ran.clone();
                draining.spawn_download(async move { after.set(after.get() + 1) });
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(ran.get(), AUTO_DOWNLOADS + 1);
            })
            .await;
    }
}
