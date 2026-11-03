//! Sending a shared contact (Desktop's `enqueueSharedContactForSend`,
//! `ts/util/enqueueSharedContactForSend.preload.ts`).

use signal_store::{MessageContent, SharedContact};

use super::*;

impl Actor {
    /// A message with only the contact (no body). Its photo is copied for
    /// the message, as Desktop's `copyAvatarForMessage` does, and a photo
    /// that can't be copied is left out.
    pub(super) async fn share_contact(self: Rc<Self>, conversation: ConversationId, share: crate::share_contact::ShareContact) {
        if !crate::remote_config().share_contact() {
            return self.error("can't share contact", "sharing contacts is off");
        }
        if !crate::share_contact::can_share(&share) {
            return self.error("can't share contact", "it needs a name and an account, or a phone number");
        }
        let mut contact = share.contact;
        contact.avatar = None;
        contact.avatar_is_profile = false;
        if let Some(option) = &share.avatar {
            match self.copy_contact_avatar(&option.path).await {
                Ok(meta) => {
                    contact.avatar = Some(meta);
                    contact.avatar_is_profile = option.is_profile;
                }
                Err(error) => warn!(error = %format!("{error:#}"), "unable to copy the shared contact's photo"),
            }
        }
        // The photo's copy goes with a message that couldn't be kept.
        let drop_copy = |contact: &SharedContact| {
            if let Some(path) = contact.avatar.as_ref().and_then(|a| a.local_path.as_ref()) {
                let _ = std::fs::remove_file(path);
            }
        };
        let Some((id, mut data, timestamp)) = self.store_outgoing(&conversation, String::new(), None, Vec::new(), None).await else {
            return drop_copy(&contact);
        };
        if let Err(error) = self.db.set_content(id, &MessageContent::Contact(Box::new(contact.clone()))).await {
            // Without its contact the message is empty, and a resend would
            // send nothing: it goes.
            self.error("failed to store the shared contact", error);
            drop_copy(&contact);
            return self.remove_message(&conversation, id).await;
        }
        self.emit(Event::MessageChanged { conversation: conversation.clone(), message_id: id });
        self.emit(Event::ConversationsChanged);
        data.contact = vec![mapping::contact_proto(&contact)];
        self.enqueue_contact(conversation, id, data, timestamp, &contact);
    }

    /// Queues a shared contact's message, with its photo to upload when
    /// it has one.
    pub(super) fn enqueue_contact(
        &self,
        conversation: ConversationId,
        message: i64,
        body: DataMessage,
        timestamp: u64,
        contact: &SharedContact,
    ) {
        match contact.avatar.as_ref().and_then(|a| Some((a.local_path.as_ref()?, a))) {
            Some((path, avatar)) => self.enqueue(Outbound::SendContact {
                conversation,
                body,
                timestamp,
                message,
                avatar: std::path::PathBuf::from(path),
                content_type: avatar.content_type.clone(),
                is_profile: contact.avatar_is_profile,
            }),
            None => self.enqueue(Outbound::Send { conversation, body: body.into(), timestamp, message: Some(message) }),
        }
    }

    /// Copies the photo next to sent attachments, typed by what it is
    /// (Desktop's `sniffImageMimeType`, JPEG when it can't tell).
    async fn copy_contact_avatar(&self, from: &str) -> anyhow::Result<AttachmentMeta> {
        let bytes = tokio::fs::read(from).await.with_context(|| format!("reading {from}"))?;
        let kind = media::ImageKind::sniff(&bytes);
        let (content_type, extension) = kind.map_or(("image/jpeg", "jpg"), |k| (k.content_type(), k.extension()));
        let path = self.config.data_dir.join("attachments").join(copy_name("contact", extension));
        tokio::fs::write(&path, &bytes).await.with_context(|| format!("writing {}", path.display()))?;
        Ok(AttachmentMeta {
            content_type: content_type.into(),
            file_name: None,
            size: bytes.len() as u64,
            width: None,
            height: None,
            local_path: Some(path.to_string_lossy().into_owned()),
            sticker: false,
            gif: false,
            voice: false,
            remote: None,
            thumbnail_path: None,
            sticker_pack: None,
        })
    }

    /// Uploads a shared contact's photo as it is, with its stored type.
    pub(super) async fn upload_contact_avatar(
        &self,
        manager: &SignalManager,
        path: &std::path::Path,
        content_type: String,
    ) -> anyhow::Result<AttachmentPointer> {
        let bytes = tokio::fs::read(path).await.with_context(|| format!("reading {}", path.display()))?;
        let spec = AttachmentSpec {
            content_type,
            length: bytes.len(),
            file_name: None,
            preview: None,
            voice_note: None,
            borderless: None,
            width: None,
            height: None,
            caption: None,
            blur_hash: None,
        };
        manager
            .upload_attachment(spec, bytes)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map_err(|e| anyhow::anyhow!("uploading the shared contact's photo: {e}"))
    }
}

/// A name for a file copied to go with a message we send: two sends in one
/// millisecond don't share it.
pub(super) fn copy_name(kind: &str, extension: &str) -> String {
    static COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COPIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("sent-{}-{n}-{kind}.{extension}", now_millis())
}
