use crate::messaging::types::{
    DeliveryEvent, ExpiryNotice, MessageError, MessageStatus, ReadAck, RelayStorageAck,
    TimeEnvelope, MAX_ENVELOPE_BYTES,
};
use ed25519_dalek::Signer;

pub struct RelayStore {
    #[allow(dead_code)]
    db: sled::Db,
    envelopes: sled::Tree,
    by_recipient: sled::Tree,
    acks: sled::Tree,
    delivered: sled::Tree,
    expiry: sled::Tree,
    status: sled::Tree,
    /// Blocked senders — key: sender_pubkey (32 bytes), value: blocked_at (i64 LE).
    block_list: sled::Tree,
}

impl RelayStore {
    pub fn open(db_dir: &str) -> Result<Self, MessageError> {
        let relay_path = format!("{}/relay", db_dir);
        let db = sled::Config::new()
            .path(&relay_path)
            .cache_capacity(32 * 1024 * 1024)
            .mode(sled::Mode::LowSpace)
            .open()
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(Self {
            envelopes: db
                .open_tree("envelopes")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            by_recipient: db
                .open_tree("by_recipient")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            acks: db
                .open_tree("acks")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            delivered: db
                .open_tree("delivered")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            expiry: db
                .open_tree("expiry")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            status: db
                .open_tree("status")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            block_list: db
                .open_tree("block_list")
                .map_err(|e| MessageError::Storage(e.to_string()))?,
            db,
        })
    }

    /// Store an inbound envelope. Idempotent — duplicate submissions are safe.
    pub fn store_envelope(&self, envelope: &TimeEnvelope) -> Result<(), MessageError> {
        let bytes = envelope.serialise()?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(MessageError::TooLarge(bytes.len(), MAX_ENVELOPE_BYTES));
        }
        self.envelopes
            .insert(envelope.msg_id, bytes.as_slice())
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        // Recipient index key: recipient_addr_hash (32) || msg_id (32)
        let mut idx_key = [0u8; 64];
        idx_key[..32].copy_from_slice(&envelope.recipient_addr_hash);
        idx_key[32..].copy_from_slice(&envelope.msg_id);
        self.by_recipient
            .insert(idx_key, &[])
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(())
    }

    /// Fetch all unexpired envelopes for a recipient.
    pub fn fetch_pending(&self, recipient_addr_hash: &[u8; 32]) -> Vec<TimeEnvelope> {
        self.by_recipient
            .scan_prefix(recipient_addr_hash.as_slice())
            .filter_map(|r| r.ok())
            .filter_map(|(key, _)| {
                if key.len() < 64 {
                    return None;
                }
                let msg_id: [u8; 32] = key[32..64].try_into().ok()?;
                let bytes = self.envelopes.get(msg_id).ok()??;
                let env = TimeEnvelope::deserialise(&bytes).ok()?;
                if env.is_expired() {
                    None
                } else {
                    Some(env)
                }
            })
            .collect()
    }

    /// Record that the recipient fetched a specific envelope.
    pub fn record_delivery(
        &self,
        msg_id: &[u8; 32],
        relay_key: &ed25519_dalek::SigningKey,
    ) -> Result<DeliveryEvent, MessageError> {
        let delivered_at = chrono::Utc::now().timestamp();
        let mut sig_input = b"time-delivery-v1".to_vec();
        sig_input.extend_from_slice(msg_id);
        sig_input.extend_from_slice(&delivered_at.to_le_bytes());
        let event = DeliveryEvent {
            msg_id: *msg_id,
            delivered_at,
            relay_pubkey: relay_key.verifying_key().to_bytes(),
            relay_sig: relay_key.sign(&sig_input).to_bytes(),
        };
        let key = format!("del:{}", hex::encode(msg_id));
        let bytes = serde_cbor::to_vec(&event).map_err(|e| MessageError::Storage(e.to_string()))?;
        self.delivered
            .insert(key.as_bytes(), bytes)
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(event)
    }

    /// Store a ReadAck after verifying the recipient's signature.
    pub fn store_ack(
        &self,
        ack: &ReadAck,
        recipient_pubkey: &[u8; 32],
    ) -> Result<(), MessageError> {
        let vk = ed25519_dalek::VerifyingKey::from_bytes(recipient_pubkey)
            .map_err(|_| MessageError::InvalidSignature)?;
        let sig_bytes = ReadAck::signing_bytes(&ack.msg_id, &ack.sender_addr, ack.read_at);
        let sig = ed25519_dalek::Signature::from_bytes(&ack.recipient_sig);
        vk.verify_strict(&sig_bytes, &sig)
            .map_err(|_| MessageError::InvalidSignature)?;

        let key = format!("ack:{}", hex::encode(ack.msg_id));
        let bytes = serde_cbor::to_vec(ack).map_err(|e| MessageError::Storage(e.to_string()))?;
        self.acks
            .insert(key.as_bytes(), bytes)
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_ack(&self, msg_id: &[u8; 32]) -> Option<ReadAck> {
        let key = format!("ack:{}", hex::encode(msg_id));
        let bytes = self.acks.get(key.as_bytes()).ok()??;
        serde_cbor::from_slice(&bytes).ok()
    }

    pub fn get_delivery(&self, msg_id: &[u8; 32]) -> Option<DeliveryEvent> {
        let key = format!("del:{}", hex::encode(msg_id));
        let bytes = self.delivered.get(key.as_bytes()).ok()??;
        serde_cbor::from_slice(&bytes).ok()
    }

    /// Sweep expired envelopes, generate ExpiryNotice records, and return them for broadcast.
    pub fn sweep_expired(&self, relay_key: &ed25519_dalek::SigningKey) -> Vec<ExpiryNotice> {
        let now = chrono::Utc::now().timestamp();

        let expired: Vec<_> = self
            .envelopes
            .iter()
            .filter_map(|r| r.ok())
            .filter_map(|(key, val)| {
                let env = TimeEnvelope::deserialise(&val).ok()?;
                if now > env.expires_at() {
                    Some((key, env))
                } else {
                    None
                }
            })
            .collect();

        let mut notices = Vec::new();
        for (key, env) in expired {
            let mut sig_input = b"time-expiry-v1".to_vec();
            sig_input.extend_from_slice(&env.msg_id);
            sig_input.extend_from_slice(&now.to_le_bytes());
            let notice = ExpiryNotice {
                msg_id: env.msg_id,
                recipient_hash: env.recipient_addr_hash,
                expired_at: now,
                relay_sig: relay_key.sign(&sig_input).to_bytes(),
            };
            if let Ok(bytes) = serde_cbor::to_vec(&notice) {
                let exp_key = format!("exp:{}", hex::encode(env.msg_id));
                let _ = self.expiry.insert(exp_key.as_bytes(), bytes);
            }
            let _ = self.envelopes.remove(&key);
            let mut idx_key = [0u8; 64];
            idx_key[..32].copy_from_slice(&env.recipient_addr_hash);
            idx_key[32..].copy_from_slice(&env.msg_id);
            let _ = self.by_recipient.remove(idx_key);
            notices.push(notice);
        }
        notices
    }

    /// Build and sign a RelayStorageAck for a successfully stored envelope.
    pub fn build_storage_ack(
        envelope: &TimeEnvelope,
        relay_key: &ed25519_dalek::SigningKey,
    ) -> Result<RelayStorageAck, MessageError> {
        let stored_at = chrono::Utc::now().timestamp();
        let mut sig_input = b"time-store-v1".to_vec();
        sig_input.extend_from_slice(&envelope.msg_id);
        sig_input.extend_from_slice(&stored_at.to_le_bytes());
        Ok(RelayStorageAck {
            msg_id: envelope.msg_id,
            stored_at,
            expires_at: envelope.expires_at(),
            relay_pubkey: relay_key.verifying_key().to_bytes(),
            relay_sig: relay_key.sign(&sig_input).to_bytes(),
        })
    }

    /// Store sender-side message status.
    pub fn set_status(
        &self,
        msg_id: &[u8; 32],
        status: &MessageStatus,
    ) -> Result<(), MessageError> {
        let bytes = serde_cbor::to_vec(status).map_err(|e| MessageError::Storage(e.to_string()))?;
        self.status
            .insert(msg_id.as_slice(), bytes)
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_status(&self, msg_id: &[u8; 32]) -> Option<MessageStatus> {
        let bytes = self.status.get(msg_id.as_slice()).ok()??;
        serde_cbor::from_slice(&bytes).ok()
    }

    /// Block a sender by their Ed25519 pubkey.
    pub fn block_sender(&self, sender_pubkey: &[u8; 32]) -> Result<(), MessageError> {
        let blocked_at = chrono::Utc::now().timestamp().to_le_bytes();
        self.block_list
            .insert(sender_pubkey.as_slice(), blocked_at.as_slice())
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Remove a block on a sender.
    pub fn unblock_sender(&self, sender_pubkey: &[u8; 32]) -> Result<(), MessageError> {
        self.block_list
            .remove(sender_pubkey.as_slice())
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Returns true if this sender's pubkey is in the block list.
    pub fn is_sender_blocked(&self, sender_pubkey: &[u8; 32]) -> bool {
        self.block_list
            .contains_key(sender_pubkey.as_slice())
            .unwrap_or(false)
    }

    /// List all blocked sender pubkeys.
    pub fn list_blocked_senders(&self) -> Vec<[u8; 32]> {
        self.block_list
            .iter()
            .filter_map(|r| r.ok())
            .filter_map(|(key, _)| key.as_ref().try_into().ok())
            .collect()
    }
}
