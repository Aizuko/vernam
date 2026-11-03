//! Sharing one of your contacts in a chat, as Signal Desktop does from the
//! composer's add menu ("Contact"): who can be picked
//! (`ts/state/smart/ShareContactDialog.preload.tsx`), the details the
//! dialog starts from and what is sent of them (`ts/util/shareContact.std.ts`),
//! and the order of the picker (`ts/util/groupByFirstInitial.std.ts`).
//! Sending is `Command::ShareContact`, as Desktop's
//! `enqueueSharedContactForSend`.

use presage::libsignal_service::prelude::phonenumber;
use presage::libsignal_service::protocol::Aci;
use signal_store::{AppDb, ContactMethod, ContactMethodKind, ConversationId, SharedContact};
use unicode_segmentation::UnicodeSegmentation as _;

/// Someone the picker lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareCandidate {
    pub id: ConversationId,
    /// Their address-book name when there is one, as Desktop's picker
    /// prefers it (`withSystemContactTitle`), else the chat's title.
    pub title: String,
    pub phone: Option<String>,
}

/// A name with its parts (Desktop's `EmbeddedContactName`); empty parts
/// are unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactName {
    pub prefix: String,
    pub given: String,
    pub middle: String,
    pub family: String,
    pub suffix: String,
}

impl ContactName {
    pub fn given(given: impl Into<String>) -> Self {
        Self { given: given.into(), ..Default::default() }
    }

    /// Given and family name combined (Desktop's `getFullName`).
    pub fn full(&self) -> Option<String> {
        signal_store::combine_names(&self.given, &self.family)
    }
}

/// A photo that can go with the contact (Desktop's `ShareContactAvatarType`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarOption {
    /// The downloaded image.
    pub path: String,
    /// Their Signal profile photo, not an address-book one.
    pub is_profile: bool,
}

/// What the dialog starts from (Desktop's `ShareContactDraftType`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareContactDraft {
    /// Their ACI, for someone registered.
    pub aci: Option<String>,
    /// The photo to send; `None` is the default (initials) avatar.
    pub avatar: Option<AvatarOption>,
    /// The photos there are: an address-book one first, then the profile's.
    pub avatar_options: Vec<AvatarOption>,
    pub name: Option<ContactName>,
    pub e164: Option<String>,
    pub organization: Option<String>,
    /// Our nickname for them: given and family name.
    pub nickname: Option<(String, String)>,
    pub note: Option<String>,
}

/// Which optional details go (Desktop's `ShareContactSelectionType`): the
/// photo and name always do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShareContactSelection {
    pub e164: bool,
    pub nickname: bool,
    pub note: bool,
}

/// What is sent: the contact, and the photo to copy and upload with it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareContact {
    pub contact: SharedContact,
    pub avatar: Option<AvatarOption>,
}

/// What we know about someone, to start a draft from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareSource {
    pub service_id: String,
    /// Their name in the phone's address book.
    pub system_name: Option<String>,
    pub profile_given: String,
    pub profile_family: String,
    pub phone: Option<String>,
    pub nickname_given: String,
    pub nickname_family: String,
    pub note: String,
    /// Their downloaded profile photo.
    pub profile_photo: Option<String>,
}

/// Desktop's `getShareContactDraft`: the address-book name before the
/// profile name, the photos there are (the first one chosen), their number,
/// nickname and note. Desktop keeps an address-book name in parts; ours is
/// one string, so it goes as the given name.
pub fn draft(source: &ShareSource) -> ShareContactDraft {
    let name = match source.system_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(system) => Some(ContactName::given(system)),
        None if !source.profile_given.trim().is_empty() || !source.profile_family.trim().is_empty() => Some(ContactName {
            given: source.profile_given.trim().to_owned(),
            family: source.profile_family.trim().to_owned(),
            ..Default::default()
        }),
        None => None,
    };
    let nickname = (!source.nickname_given.trim().is_empty() || !source.nickname_family.trim().is_empty())
        .then(|| (source.nickname_given.trim().to_owned(), source.nickname_family.trim().to_owned()));
    // Only a downloaded photo can go (Desktop skips one without a `path`).
    let avatar_options: Vec<AvatarOption> = source
        .profile_photo
        .iter()
        .filter(|p| !p.is_empty())
        .map(|path| AvatarOption { path: path.clone(), is_profile: true })
        .collect();
    ShareContactDraft {
        aci: registered_aci(&source.service_id),
        avatar: avatar_options.first().cloned(),
        avatar_options,
        name,
        e164: source.phone.clone().filter(|p| !p.trim().is_empty()),
        organization: None,
        nickname,
        note: Some(source.note.clone()).filter(|n| !n.trim().is_empty()),
    }
}

/// Desktop's `getRegisteredAci`: an ACI, not a PNI. Desktop also leaves out
/// someone it has seen unregistered; we don't keep that.
fn registered_aci(service_id: &str) -> Option<String> {
    Aci::parse_from_service_id_string(service_id).map(|_| service_id.to_owned())
}

/// Desktop's `withE164AsMissingName`: with a number but no name, the
/// formatted number is the name.
pub fn with_e164_as_missing_name(draft: ShareContactDraft, our_phone: Option<&str>) -> ShareContactDraft {
    match (&draft.name, &draft.e164) {
        (None, Some(e164)) => {
            let name = ContactName::given(format_phone(e164, our_phone));
            ShareContactDraft { name: Some(name), ..draft }
        }
        _ => draft,
    }
}

/// Desktop's `getDefaultShareContactSelection`: the number when there is
/// one; never the nickname or the note.
pub fn default_selection(draft: &ShareContactDraft) -> ShareContactSelection {
    ShareContactSelection { e164: draft.e164.is_some(), nickname: false, note: false }
}

/// Desktop's `applyShareContactSelection` and the contact
/// `enqueueSharedContactForSend` builds from it: the number as a custom
/// one, the photo and name always.
pub fn apply_selection(draft: &ShareContactDraft, selection: ShareContactSelection) -> ShareContact {
    let name = draft.name.clone().unwrap_or_default();
    let (nickname_given, nickname_family) = draft.nickname.clone().filter(|_| selection.nickname).unwrap_or_default();
    let contact = SharedContact {
        given_name: name.given,
        family_name: name.family,
        prefix: name.prefix,
        suffix: name.suffix,
        middle_name: name.middle,
        aci: draft.aci.clone(),
        signal_nickname_given: nickname_given,
        signal_nickname_family: nickname_family,
        note: draft.note.clone().filter(|_| selection.note),
        numbers: draft
            .e164
            .iter()
            .filter(|_| selection.e164)
            .map(|e164| ContactMethod { value: e164.clone(), kind: ContactMethodKind::Custom, label: None })
            .collect(),
        organization: draft.organization.clone(),
        avatar_is_profile: draft.avatar.as_ref().is_some_and(|a| a.is_profile),
        ..Default::default()
    };
    ShareContact { contact, avatar: draft.avatar.clone() }
}

/// Desktop's `canShareContact`: a name (or organization) and their Signal
/// account, or a number.
pub fn can_share(share: &ShareContact) -> bool {
    let contact = &share.contact;
    let display = contact.display_name().or_else(|| contact.organization.clone().filter(|o| !o.trim().is_empty()));
    (display.is_some() && contact.aci.is_some()) || !contact.numbers.is_empty()
}

/// Desktop's `PhoneNumber.format`: national for a number in our own
/// region, international otherwise, and as it is when it doesn't parse.
/// Where a region can't be told (a made-up number), the same country code
/// counts as our region.
pub fn format_phone(e164: &str, our_phone: Option<&str>) -> String {
    let Ok(number) = phonenumber::parse(None, e164) else { return e164.to_owned() };
    let ours = our_phone.and_then(|p| phonenumber::parse(None, p).ok());
    let same_region = ours.is_some_and(|ours| match (ours.country().id(), number.country().id()) {
        (Some(a), Some(b)) => a == b,
        _ => ours.code().value() == number.code().value(),
    });
    let mode = if same_region { phonenumber::Mode::National } else { phonenumber::Mode::International };
    number.format().mode(mode).to_string()
}

/// Desktop's `groupByFirstInitial`: sections by each title's first letter
/// (combining marks kept with it), uppercased, in order, with titles that
/// have no letter in a last section under "". Items keep their order
/// within a section. Desktop orders initials with `localeCompare`; here
/// they're compared by their lowercase text.
pub fn group_by_first_initial<T>(items: Vec<T>, title: impl Fn(&T) -> &str) -> Vec<(String, Vec<T>)> {
    let mut sections: Vec<(String, Vec<T>)> = Vec::new();
    for item in items {
        let initial = first_initial(title(&item));
        match sections.iter_mut().find(|(i, _)| *i == initial) {
            Some((_, section)) => section.push(item),
            None => sections.push((initial, vec![item])),
        }
    }
    sections.sort_by(|(a, _), (b, _)| match (a.is_empty(), b.is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)),
    });
    sections
}

/// The first grapheme that starts with a letter, uppercased.
fn first_initial(title: &str) -> String {
    title.graphemes(true).find(|g| g.chars().next().is_some_and(char::is_alphabetic)).map(str::to_uppercase).unwrap_or_default()
}

/// The picker's order (Desktop's `SelectContact`): by title, then grouped
/// by first initial.
pub fn sort_candidates(mut candidates: Vec<ShareCandidate>) -> Vec<ShareCandidate> {
    candidates.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then_with(|| a.title.cmp(&b.title)));
    group_by_first_initial(candidates, |c| c.title.as_str()).into_iter().flat_map(|(_, items)| items).collect()
}

/// Whether a candidate matches the search: words of its title, or the
/// digits of its number. Desktop searches with `filterAndSortConversations`
/// (fuzzy, over more fields); this is a plain match.
pub fn matches(candidate: &ShareCandidate, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    if candidate.title.to_lowercase().contains(&query) {
        return true;
    }
    // A number's digits, only when the search reads as one ("Bob 2" is a
    // name).
    if query.chars().any(char::is_alphabetic) {
        return false;
    }
    let digits: String = query.chars().filter(char::is_ascii_digit).collect();
    !digits.is_empty()
        && candidate.phone.as_deref().is_some_and(|p| p.chars().filter(char::is_ascii_digit).collect::<String>().contains(&digits))
}

/// Who can be shared in `recipient` (Desktop's `isShareableContact`): our
/// Signal connections (people we share our profile with or have in our
/// address book) with a name and an account or a number, and anyone else
/// with a number. Not the chat's own person, us, the Signal chat or anyone
/// blocked (`people` leaves those out).
pub async fn candidates(db: &AppDb, recipient: &ConversationId) -> Vec<ShareCandidate> {
    let people = db.people().await.unwrap_or_default();
    let system: std::collections::HashSet<String> = db.system_contacts().await.unwrap_or_default().into_iter().collect();
    let mut out = Vec::new();
    for person in people {
        let Some(sid) = person.id.as_direct().map(str::to_owned) else { continue };
        if person.id == *recipient {
            continue;
        }
        let phone = person.phone.clone().filter(|p| !p.trim().is_empty());
        let system_name = db.contact_name(&sid).await.ok().flatten().filter(|n| !n.trim().is_empty());
        let connection = system.contains(&sid) || db.profile_sharing(&person.id).await.unwrap_or(false);
        let shareable = phone.is_some()
            || (connection && registered_aci(&sid).is_some() && {
                // Desktop's `canShareContactFromConversation`: a real name,
                // from the address book or the profile.
                let profile = db.person_profile(&sid).await.ok().flatten();
                system_name.is_some() || profile.and_then(|p| p.profile_name()).is_some()
            });
        if shareable {
            out.push(ShareCandidate { id: person.id, title: system_name.unwrap_or(person.title), phone });
        }
    }
    sort_candidates(out)
}

/// The draft for a picked candidate, from what the database knows.
pub async fn read_draft(db: &AppDb, candidate: &ShareCandidate) -> ShareContactDraft {
    let Some(sid) = candidate.id.as_direct() else { return ShareContactDraft::default() };
    let conversation = db.conversation(&candidate.id).await.ok().flatten();
    let profile = db.person_profile(sid).await.ok().flatten().unwrap_or_default();
    let nickname = db.contact_nickname(sid).await.ok().flatten().unwrap_or_default();
    // Their profile photo, which someone we've no chat with (from a group)
    // has too; else the chat's.
    let photo = db
        .person_avatar(sid)
        .await
        .ok()
        .flatten()
        .into_iter()
        .chain(conversation.as_ref().and_then(|c| c.avatar_path.clone()))
        .find(|p| std::path::Path::new(p).is_file());
    draft(&ShareSource {
        service_id: sid.to_owned(),
        system_name: db.contact_name(sid).await.ok().flatten(),
        profile_given: profile.given_name,
        profile_family: profile.family_name,
        phone: conversation.and_then(|c| c.phone).or_else(|| candidate.phone.clone()),
        nickname_given: nickname.given,
        nickname_family: nickname.family,
        note: nickname.note,
        profile_photo: photo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACI: &str = "00000000-0000-4000-8000-0000000000f0";

    fn sample() -> ShareContactDraft {
        let avatar = AvatarOption { path: "/tmp/avatar.jpg".into(), is_profile: false };
        ShareContactDraft {
            aci: Some(ACI.into()),
            avatar: Some(avatar.clone()),
            avatar_options: vec![avatar],
            name: Some(ContactName { given: "Paige".into(), family: "Hall".into(), ..Default::default() }),
            organization: Some("Signal".into()),
            e164: Some("+15101234567".into()),
            nickname: Some(("P".into(), String::new())),
            note: Some("Met in 2017".into()),
        }
    }

    #[test]
    fn only_selected_fields_go_and_the_photo_and_name_always_do() {
        let draft = sample();
        let share = apply_selection(&draft, ShareContactSelection { e164: false, nickname: false, note: true });
        assert_eq!(share.contact.display_name().as_deref(), Some("Paige Hall"));
        assert_eq!(share.contact.organization.as_deref(), Some("Signal"));
        assert_eq!(share.contact.note.as_deref(), Some("Met in 2017"));
        assert!(share.contact.numbers.is_empty());
        assert_eq!(share.contact.display_nickname(), None);
        assert_eq!(share.avatar, draft.avatar);
        let all = apply_selection(&draft, ShareContactSelection { e164: true, nickname: true, note: false });
        assert_eq!(all.contact.numbers, [ContactMethod { value: "+15101234567".into(), kind: ContactMethodKind::Custom, label: None }]);
        assert_eq!(all.contact.display_nickname().as_deref(), Some("P"));
        assert_eq!(all.contact.note, None);
        assert_eq!(all.contact.aci.as_deref(), Some(ACI));
    }

    #[test]
    fn the_number_is_the_name_when_there_is_none() {
        let draft = ShareContactDraft { name: None, ..sample() };
        let named = with_e164_as_missing_name(draft, Some("+14155550100"));
        assert_eq!(named.name, Some(ContactName::given("(510) 123-4567")));
        // From another country: international.
        let draft = ShareContactDraft { name: None, ..sample() };
        let named = with_e164_as_missing_name(draft, Some("+442079460958"));
        assert_eq!(named.name, Some(ContactName::given("+1 510-123-4567")));
        // A name stays.
        assert_eq!(with_e164_as_missing_name(sample(), None), sample());
    }

    #[test]
    fn sharing_needs_a_name_and_an_account_or_a_number() {
        let empty = ShareContactDraft { aci: Some(ACI.into()), ..Default::default() };
        let none = ShareContactSelection::default();
        let named = ShareContactDraft { name: Some(ContactName::given("Paige")), ..empty.clone() };
        assert!(can_share(&apply_selection(&named, none)));
        let by_number = ShareContactDraft { aci: None, name: Some(ContactName::given("+15101234567")), e164: Some("+15101234567".into()), ..Default::default() };
        assert!(can_share(&apply_selection(&by_number, ShareContactSelection { e164: true, ..none })));
        // The number unchecked and no account: nothing to share.
        assert!(!can_share(&apply_selection(&by_number, none)));
        let no_account = ShareContactDraft { aci: None, ..named };
        assert!(!can_share(&apply_selection(&no_account, none)));
        // An organization is a name.
        let organization = ShareContactDraft { organization: Some("Signal".into()), ..empty };
        assert!(can_share(&apply_selection(&organization, none)));
    }

    #[test]
    fn drafts_start_from_the_address_book_then_the_profile() {
        let source = ShareSource {
            service_id: ACI.into(),
            system_name: Some("System Name".into()),
            profile_given: "Profile".into(),
            profile_family: "Name".into(),
            ..Default::default()
        };
        assert_eq!(draft(&source).name, Some(ContactName::given("System Name")));
        let profile = ShareSource { system_name: None, ..source.clone() };
        assert_eq!(draft(&profile).name, Some(ContactName { given: "Profile".into(), family: "Name".into(), ..Default::default() }));
        assert_eq!(draft(&ShareSource { service_id: ACI.into(), ..Default::default() }).name, None);
    }

    #[test]
    fn drafts_offer_a_downloaded_photo_and_copy_the_details() {
        let source = ShareSource {
            service_id: ACI.into(),
            phone: Some("+15101234567".into()),
            nickname_given: "P".into(),
            note: "Met in 2017".into(),
            profile_photo: Some("/tmp/profile.jpg".into()),
            ..Default::default()
        };
        let draft = draft(&source);
        assert_eq!(draft.avatar_options, [AvatarOption { path: "/tmp/profile.jpg".into(), is_profile: true }]);
        assert_eq!(draft.avatar, draft.avatar_options.first().cloned());
        assert_eq!(draft.e164.as_deref(), Some("+15101234567"));
        assert_eq!(draft.nickname, Some(("P".into(), String::new())));
        assert_eq!(draft.note.as_deref(), Some("Met in 2017"));
        assert_eq!(draft.aci.as_deref(), Some(ACI));
        assert_eq!(default_selection(&draft), ShareContactSelection { e164: true, nickname: false, note: false });
        // A PNI isn't an account to send.
        let pni = ShareSource { service_id: format!("PNI:{ACI}"), ..source };
        assert_eq!(super::draft(&pni).aci, None);
        assert_eq!(super::draft(&ShareSource::default()).avatar, None);
    }

    #[test]
    fn grouping_by_first_initial_follows_desktop() {
        let group = |items: &[&'static str]| group_by_first_initial(items.to_vec(), |s| s);
        assert!(group(&[]).is_empty());
        assert_eq!(
            group(&["charlie", "Abby", "andrew", "Casey"]),
            [("A".to_owned(), vec!["Abby", "andrew"]), ("C".to_owned(), vec!["charlie", "Casey"])]
        );
        // Leading non-letters are skipped.
        assert_eq!(group(&["123 Bob", "🎉 Alice"]), [("A".to_owned(), vec!["🎉 Alice"]), ("B".to_owned(), vec!["123 Bob"])]);
        // No letter at all: a last section.
        assert_eq!(group(&["123", "Zed", ""]), [("Z".to_owned(), vec!["Zed"]), (String::new(), vec!["123", ""])]);
        // A combining mark stays with its letter.
        assert_eq!(group(&["e\u{301}clair"]), [("E\u{301}".to_owned(), vec!["e\u{301}clair"])]);
    }

    #[test]
    fn the_picker_sorts_and_searches() {
        let candidate = |title: &str, phone: Option<&str>| ShareCandidate {
            id: ConversationId::direct(title),
            title: title.into(),
            phone: phone.map(Into::into),
        };
        let sorted = sort_candidates(vec![candidate("+1 555 0100", None), candidate("leo", None), candidate("Ana", None)]);
        let titles: Vec<_> = sorted.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Ana", "leo", "+1 555 0100"]);
        let maya = candidate("Maya Chen", Some("+1 555 0100"));
        assert!(matches(&maya, "chen"));
        assert!(matches(&maya, "0100"));
        assert!(!matches(&maya, "leo 0"));
        assert!(!matches(&maya, "leo"));
        assert!(matches(&maya, "  "));
    }
}
