//! Photos of people and groups we haven't accepted, as Signal Desktop
//! withholds them: a profile photo downloads only when their 1:1 chat is
//! accepted or we share an accepted group with them (`setAndMaybeFetch
//! ProfileAvatar` in `ts/models/conversations.preload.ts`, the group part
//! from commit c1e8345), and a group's only when the group is accepted
//! (`applyNewAvatar` in `ts/groups.preload.ts`). Otherwise only the photo's
//! path is kept (`remoteAvatarUrl`, here [`signal_store::REMOTE_AVATAR`]),
//! and it downloads when the chat is accepted (`applyMessageRequestResponse`)
//! or when asked to ("View", `startAvatarDownload` in
//! `ts/state/ducks/conversations.preload.ts`).

use super::*;

/// Settings key: "1" once the photos downloaded before any were withheld,
/// of people and groups that wouldn't have been downloaded, were dropped.
const SETTING_UNTRUSTED_DROPPED: &str = "avatars.untrusted_dropped";
/// Withheld photos downloaded per sync because their chat came to be
/// trusted; the rest wait for the next.
const NOW_TRUSTED_PER_SYNC: usize = 30;

fn withheld_key(conversation: &ConversationId) -> String {
    format!("{}{}", signal_store::REMOTE_AVATAR, conversation.0)
}

/// Whether a person's new photo downloads (Desktop's `shouldDownload`):
/// when asked to, ours, or theirs once trusted ([`AppDb::photo_trusted`]).
pub(super) fn person_photo_downloads(force: bool, ours: bool, trusted: bool) -> bool {
    force || ours || trusted
}

/// Whether a group's new photo downloads: when asked to, or the group is
/// accepted, which an invitation isn't (Desktop's `areWePending ||
/// !isConversationAccepted` withholds it).
pub(super) fn group_photo_downloads(force: bool, accepted: bool) -> bool {
    force || accepted
}

/// Whether answering a request downloads the photo it held back.
pub(super) fn answer_downloads_photo(answer: signal_store::MessageRequestResponse, withheld: bool) -> bool {
    answer == signal_store::MessageRequestResponse::Accept && withheld
}

impl Actor {
    /// Keeps `path` as the chat's withheld photo; whether it's new.
    pub(super) async fn withhold_photo(&self, conversation: &ConversationId, path: &str) -> bool {
        let key = withheld_key(conversation);
        if self.db.setting(&key).await.ok().flatten().as_deref() == Some(path) {
            return false;
        }
        match self.db.set_setting(&key, path).await {
            Ok(()) => true,
            Err(error) => {
                warn!(%error, "failed to note a withheld photo");
                false
            }
        }
    }

    /// The chat's photo isn't withheld (anymore); whether it was.
    pub(super) async fn forget_withheld_photo(&self, conversation: &ConversationId) -> bool {
        let key = withheld_key(conversation);
        if self.db.setting(&key).await.ok().flatten().is_none() {
            return false;
        }
        self.db.remove_setting(&key).await.is_ok()
    }

    /// Downloads a chat's withheld photo regardless (Desktop's
    /// `startAvatarDownload`, and accepting a request): a person's from the
    /// path we kept, a group's from the group.
    pub(super) async fn download_withheld_photo(self: &Rc<Self>, conversation: &ConversationId) {
        let Some(manager) = self.manager() else { return };
        if let Some(key) = conversation.as_group().and_then(|k| <[u8; 32]>::try_from(k).ok()) {
            if self.apply_group_photo(&manager, key, false, true).await {
                self.emit(Event::ConversationsChanged);
            }
            return;
        }
        let Some(sid) = conversation.as_direct() else { return };
        // Desktop: "no avatar URL" when there's nothing withheld.
        let Some(path) = self.db.setting(&withheld_key(conversation)).await.ok().flatten() else { return };
        let Some(aci) = Aci::parse_from_service_id_string(sid) else { return };
        let Some(key) = self.profile_key_of(&ServiceId::from(aci)).await else {
            return warn!("can't download a withheld photo: no profile key");
        };
        let dir = self.config.data_dir.join("avatars");
        if let Err(error) = std::fs::create_dir_all(&dir) {
            return warn!(%error, "failed to create avatars directory");
        }
        let saved = self.db.person_avatars().await.unwrap_or_default().into_iter().find(|(s, _)| s == sid).map(|(_, p)| p);
        let outcome = self.apply_person_photo(&manager, sid, Some(path), key, saved.as_deref(), &dir, true).await;
        if outcome.changed() {
            self.emit(Event::ConversationsChanged);
            self.emit(Event::ProfileChanged(sid.to_owned()));
        } else if outcome == PhotoOutcome::DownloadFailed {
            warn!("withheld photo download failed");
        }
    }

    /// Downloads the withheld photos of people who came to be trusted
    /// since (a message of ours, an accepted group), a bounded number per
    /// sync. Desktop gets these at its next profile fetch; the path is kept
    /// here, so no fetch is needed. Returns whether anything changed and
    /// whether more are waiting.
    pub(super) async fn download_now_trusted(&self, manager: &SignalManager, dir: &std::path::Path) -> (bool, bool) {
        let trusted = match self.db.withheld_photos_now_trusted().await {
            Ok(trusted) => trusted,
            Err(error) => {
                warn!(%error, "couldn't read the withheld photos");
                return (false, false);
            }
        };
        // One that failed this session waits for the next, so it doesn't
        // take a turn from the rest every sync.
        let failures = self.trusted_photo_failures.borrow().clone();
        let trusted: Vec<_> = trusted.into_iter().filter(|(sid, _)| !failures.contains(sid)).collect();
        if trusted.is_empty() {
            return (false, false);
        }
        let saved: HashMap<String, String> = self.db.person_avatars().await.unwrap_or_default().into_iter().collect();
        let mut changed = false;
        let mut outcomes: HashMap<&'static str, usize> = HashMap::new();
        let more = trusted.len() > NOW_TRUSTED_PER_SYNC;
        for (sid, path) in trusted.into_iter().take(NOW_TRUSTED_PER_SYNC) {
            let key = match Aci::parse_from_service_id_string(&sid) {
                Some(aci) => self.profile_key_of(&ServiceId::from(aci)).await,
                None => None,
            };
            let outcome = match key {
                Some(key) => self.apply_person_photo(manager, &sid, Some(path), key, saved.get(&sid).map(String::as_str), dir, false).await,
                None => PhotoOutcome::NoKey,
            };
            changed |= outcome.changed();
            if matches!(outcome, PhotoOutcome::DownloadFailed | PhotoOutcome::NoKey) {
                self.trusted_photo_failures.borrow_mut().insert(sid);
            }
            *outcomes.entry(outcome.label()).or_default() += 1;
        }
        info!(?outcomes, more, "withheld profile photos, now trusted");
        // The rest follow in the next batch, as profile photo checks do.
        (changed, more)
    }

    /// Once: drops the profile photos downloaded before photos were
    /// withheld, of people whose photos wouldn't have been downloaded, so
    /// they're shown as withheld, as Desktop would show them. A person's
    /// photo path comes back at their next check, made soon. Each is
    /// forgotten before its file goes, and the pass counts as done only
    /// when every one was. Groups' photos stay: one joined by link before
    /// joining shared our profile would count as not accepted. Whether
    /// anything was dropped.
    pub(super) async fn drop_untrusted_photos(&self) -> bool {
        if self.db.setting(SETTING_UNTRUSTED_DROPPED).await.ok().flatten().is_some() {
            return false;
        }
        let me = self.self_aci();
        let Ok(people) = self.db.person_avatars().await else { return false };
        let (mut dropped, mut failed) = (0usize, 0usize);
        for (sid, path) in people {
            if sid == me {
                continue;
            }
            match self.db.photo_trusted(&sid).await {
                Ok(true) => continue,
                Ok(false) => {}
                // Tried again at the next sync.
                Err(error) => {
                    warn!(%error, "couldn't tell whose photos to drop");
                    return dropped > 0;
                }
            }
            if let Err(error) = self.db.remove_person_avatar(&sid).await {
                warn!(%error, "couldn't forget a photo to drop");
                failed += 1;
                continue;
            }
            let _ = std::fs::remove_file(&path);
            let chat = ConversationId::direct(&sid);
            if let Some(old) = self.db.conversation(&chat).await.ok().flatten().and_then(|c| c.avatar_path) {
                match self.db.set_avatar(&chat, None).await {
                    Ok(_) => {
                        let _ = std::fs::remove_file(old);
                    }
                    Err(error) => {
                        warn!(%error, "couldn't forget a chat's photo to drop");
                        failed += 1;
                    }
                }
            }
            // Checked again soon, which keeps the photo's path.
            let _ = self.db.remove_setting(&format!("{SETTING_AVATAR_CHECKED}{sid}")).await;
            dropped += 1;
        }
        if failed == 0 {
            let _ = self.db.set_setting(SETTING_UNTRUSTED_DROPPED, "1").await;
        }
        info!(dropped, failed, "dropped photos of people not accepted");
        dropped > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use signal_store::MessageRequestResponse as Answer;

    #[test]
    fn a_persons_photo_downloads_once_trusted_ours_or_asked_for() {
        // Trusted: an accepted 1:1 chat or a shared accepted group (which
        // `AppDb::photo_trusted` decides, tested with the store).
        assert!(person_photo_downloads(false, false, true));
        // A request, someone blocked, someone only in a group we haven't
        // accepted: withheld.
        assert!(!person_photo_downloads(false, false, false));
        // "View", or accepting: downloaded regardless.
        assert!(person_photo_downloads(true, false, false));
        // Ours always.
        assert!(person_photo_downloads(false, true, false));
    }

    #[test]
    fn a_groups_photo_downloads_once_accepted_or_asked_for() {
        assert!(group_photo_downloads(false, true));
        // Not accepted, or only invited.
        assert!(!group_photo_downloads(false, false));
        assert!(group_photo_downloads(true, false));
    }

    #[test]
    fn accepting_downloads_the_photo_held_back() {
        assert!(answer_downloads_photo(Answer::Accept, true));
        assert!(!answer_downloads_photo(Answer::Accept, false), "nothing held back");
        for answer in [Answer::Block, Answer::Delete, Answer::BlockAndDelete, Answer::Spam, Answer::BlockAndSpam] {
            assert!(!answer_downloads_photo(answer, true), "{answer:?}");
        }
    }

    #[test]
    fn withheld_photos_are_kept_by_chat() {
        assert_eq!(withheld_key(&ConversationId::direct("bob")), "avatar.remote.d:bob");
        assert_eq!(withheld_key(&ConversationId::group(&[0xab; 32])), format!("avatar.remote.g:{}", "ab".repeat(32)));
    }
}
