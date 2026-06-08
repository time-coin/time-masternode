use crate::messaging::contacts::{Contact, ContactsBook};
use crate::messaging::crypto::{decrypt_envelope, encrypt_message};
use crate::messaging::relay::RelayStore;
use crate::messaging::types::{
    MessageError, MessageStatus, TimeMessage, MAX_BODY_BYTES, MAX_SUBJECT_BYTES, MAX_TTL_SECONDS,
    MSG_VERSION, RELAY_REPLICATION_FACTOR,
};
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::utxo_manager::UTXOStateManager;
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use sha2::Digest;
use std::sync::Arc;

/// Resolve a recipient's Ed25519 pubkey using the 3-source priority chain.
pub async fn resolve_recipient_pubkey(
    address: &str,
    contacts: &ContactsBook,
    utxo_mgr: &Arc<UTXOStateManager>,
    peer_registry: &Arc<PeerConnectionRegistry>,
) -> Result<[u8; 32], MessageError> {
    // 1. Local contacts book
    if let Some(contact) = contacts.get(address) {
        tracing::debug!("📨 Pubkey resolved from contacts book for {}", address);
        return Ok(contact.pubkey);
    }

    // 2. Local UTXO pubkey cache
    if let Some(pk) = utxo_mgr.find_pubkey_for_address(address) {
        tracing::debug!("📨 Pubkey resolved from UTXO cache for {}", address);
        let _ = contacts.upsert(
            address,
            Contact {
                pubkey: pk,
                label: None,
                added_at: chrono::Utc::now().timestamp(),
            },
        );
        return Ok(pk);
    }

    // 3. P2P MsgPubkeyQuery (privacy-preserving hash)
    let addr_hash: [u8; 32] = sha2::Sha256::digest(address.as_bytes()).into();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<[u8; 32]>();
    peer_registry.pending_pubkey_queries.insert(addr_hash, tx);

    peer_registry
        .broadcast(NetworkMessage::MsgPubkeyQuery {
            address_hash: addr_hash,
        })
        .await;

    let result = tokio::time::timeout(tokio::time::Duration::from_secs(5), rx.recv()).await;
    peer_registry.pending_pubkey_queries.remove(&addr_hash);

    match result {
        Ok(Some(pk)) => {
            tracing::debug!("📨 Pubkey resolved via P2P for {}", address);
            let _ = contacts.upsert(
                address,
                Contact {
                    pubkey: pk,
                    label: None,
                    added_at: chrono::Utc::now().timestamp(),
                },
            );
            Ok(pk)
        }
        _ => Err(MessageError::PubkeyNotFound(address.to_string())),
    }
}

/// Deterministically select up to `n` relay peers from the Silver/Gold set.
/// Uses SHA-256(msg_id || peer_pubkey) for consistent ordering.
fn select_relay_peers(
    candidates: &[(String, [u8; 32])], // (ip_or_addr, pubkey)
    msg_id: &[u8; 32],
    n: usize,
) -> Vec<String> {
    let mut scored: Vec<(u32, &str)> = candidates
        .iter()
        .map(|(addr, pubkey)| {
            let mut input = Vec::with_capacity(32 + 32);
            input.extend_from_slice(msg_id);
            input.extend_from_slice(pubkey);
            let hash: [u8; 32] = sha2::Sha256::digest(&input).into();
            let score = u32::from_le_bytes(hash[0..4].try_into().unwrap_or([0; 4]));
            (score, addr.as_str())
        })
        .collect();
    scored.sort_by_key(|(s, _)| *s);
    scored
        .into_iter()
        .take(n)
        .map(|(_, a)| a.to_string())
        .collect()
}

/// `sendmessage` RPC implementation.
pub async fn rpc_send_message(
    params: &Value,
    wallet_key: &SigningKey,
    relay_store: &Arc<RelayStore>,
    contacts: &ContactsBook,
    utxo_mgr: &Arc<UTXOStateManager>,
    peer_registry: &Arc<PeerConnectionRegistry>,
    registry: &Arc<crate::masternode_registry::MasternodeRegistry>,
    _network: crate::network_type::NetworkType,
) -> Result<Value, String> {
    let to = params
        .get("to")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'to' parameter")?;

    let subject = params
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .as_bytes()
        .to_vec();

    let body_str = params
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'body' parameter")?;

    let body = body_str.as_bytes().to_vec();
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "body too large: {} bytes (max {})",
            body.len(),
            MAX_BODY_BYTES
        ));
    }
    if subject.len() > MAX_SUBJECT_BYTES {
        return Err(format!(
            "subject too long (max {} bytes)",
            MAX_SUBJECT_BYTES
        ));
    }

    let ttl_hours = params
        .get("ttl_hours")
        .and_then(|v| v.as_u64())
        .unwrap_or(720);
    let ttl_seconds = (ttl_hours * 3600).min(MAX_TTL_SECONDS as u64) as u32;

    let request_receipt = params
        .get("request_read_receipt")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flags: u8 = if request_receipt { 0x01 } else { 0x00 };

    // Resolve recipient pubkey
    let recipient_pubkey = resolve_recipient_pubkey(to, contacts, utxo_mgr, peer_registry)
        .await
        .map_err(|e| e.to_string())?;

    // Build and encrypt plaintext message
    let plaintext = TimeMessage {
        version: MSG_VERSION,
        sender_pubkey: wallet_key.verifying_key().to_bytes(),
        recipient_addr: to.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        ttl_seconds,
        flags,
        thread_id: None,
        subject,
        body,
    };
    let plaintext_bytes =
        serde_cbor::to_vec(&plaintext).map_err(|e| format!("serialise error: {e}"))?;
    let envelope = encrypt_message(
        wallet_key,
        &recipient_pubkey,
        &plaintext_bytes,
        to,
        ttl_seconds,
        flags,
    )
    .map_err(|e| e.to_string())?;
    let msg_id = envelope.msg_id;
    let envelope_bytes = envelope.serialise().map_err(|e| e.to_string())?;

    // Select relay peers (Silver/Gold masternodes)
    let silver_gold: Vec<(String, [u8; 32])> = registry
        .list_all()
        .await
        .into_iter()
        .filter(|info| {
            matches!(
                info.masternode.tier,
                crate::types::MasternodeTier::Silver | crate::types::MasternodeTier::Gold
            )
        })
        .map(|info| {
            let pubkey = info.masternode.public_key.to_bytes();
            (info.masternode.address.clone(), pubkey)
        })
        .collect();

    let relay_targets = select_relay_peers(&silver_gold, &msg_id, RELAY_REPLICATION_FACTOR);

    if relay_targets.is_empty() {
        return Err("No Silver/Gold relay nodes available".to_string());
    }

    // Register pending ack listeners and broadcast
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    peer_registry.pending_relay_acks.insert(msg_id, tx);

    for target in &relay_targets {
        let _ = peer_registry
            .send_to_peer(
                target,
                NetworkMessage::MsgSubmit {
                    envelope: envelope_bytes.clone(),
                },
            )
            .await;
    }

    // Wait for 2-of-3 acks within 10 seconds
    let required = (relay_targets.len() + 1) / 2 + if relay_targets.len() == 1 { 0 } else { 0 };
    let required = required.max(1).min(2);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    let mut ack_count = 0usize;

    while ack_count < required {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(_)) => ack_count += 1,
            _ => break,
        }
    }

    peer_registry.pending_relay_acks.remove(&msg_id);

    let status = if ack_count >= required {
        MessageStatus::Pending
    } else if ack_count > 0 {
        MessageStatus::Pending // partial — still attempt delivery
    } else {
        MessageStatus::Failed
    };

    let _ = relay_store.set_status(&msg_id, &status);

    let msg_id_hex = hex::encode(msg_id);
    Ok(json!({
        "msg_id": msg_id_hex,
        "status": status.as_str(),
        "relay_acks": ack_count,
        "relay_targets": relay_targets.len()
    }))
}

/// `getmessages` RPC implementation.
pub async fn rpc_get_messages(
    params: &Value,
    wallet_key: &SigningKey,
    relay_store: &Arc<RelayStore>,
    contacts: &ContactsBook,
    peer_registry: &Arc<PeerConnectionRegistry>,
    network: crate::network_type::NetworkType,
) -> Result<Value, String> {
    let since = params.get("since").and_then(|v| v.as_i64()).unwrap_or(0);

    let wallet_addr =
        crate::address::Address::from_public_key(wallet_key.verifying_key().as_bytes(), network)
            .to_string();
    let recipient_addr_hash: [u8; 32] = sha2::Sha256::digest(wallet_addr.as_bytes()).into();

    // Collect envelopes from local relay store (Silver/Gold) + network
    let local_envelopes = relay_store.fetch_pending(&recipient_addr_hash);
    let mut all_envelope_bytes: Vec<Vec<u8>> = local_envelopes
        .iter()
        .filter_map(|e| e.serialise().ok())
        .collect();

    // Also query peers for any envelopes they hold
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    peer_registry
        .pending_msg_envelopes
        .insert(recipient_addr_hash, tx);

    peer_registry
        .broadcast(NetworkMessage::MsgFetchPending {
            recipient_addr_hash,
            since,
        })
        .await;

    // Collect responses for 3 seconds
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(env_bytes)) => all_envelope_bytes.push(env_bytes),
            _ => break,
        }
    }
    peer_registry
        .pending_msg_envelopes
        .remove(&recipient_addr_hash);

    // Deduplicate by msg_id
    let mut seen_ids = std::collections::HashSet::new();
    let unique: Vec<_> = all_envelope_bytes
        .into_iter()
        .filter_map(|bytes| {
            crate::messaging::types::TimeEnvelope::deserialise(&bytes)
                .ok()
                .map(|e| (e, bytes))
        })
        .filter(|(env, _)| seen_ids.insert(env.msg_id))
        .collect();

    let mut messages = Vec::new();
    for (envelope, _) in &unique {
        match decrypt_envelope(wallet_key, envelope) {
            Ok(msg) => {
                // Derive the sender's TIME address from their pubkey
                let sender_addr =
                    crate::address::Address::from_public_key(&envelope.sender_pubkey, network)
                        .to_string();

                // Auto-add sender to contacts so future sends require no network lookup
                let _ = contacts.upsert(
                    &sender_addr,
                    Contact {
                        pubkey: envelope.sender_pubkey,
                        label: None,
                        added_at: chrono::Utc::now().timestamp(),
                    },
                );

                // If sender requested a read receipt, send one
                if msg.request_read_receipt() {
                    let _ = send_read_ack(envelope, &sender_addr, wallet_key, peer_registry).await;
                }

                let msg_json = json!({
                    "msg_id": hex::encode(&envelope.msg_id),
                    "from": sender_addr,
                    "subject": String::from_utf8_lossy(&msg.subject),
                    "body": String::from_utf8_lossy(&msg.body),
                    "timestamp": msg.timestamp,
                    "ttl_seconds": msg.ttl_seconds,
                    "read_receipt_requested": msg.request_read_receipt(),
                });
                messages.push(msg_json);
            }
            Err(e) => {
                tracing::debug!(
                    "📨 Could not decrypt envelope {}: {}",
                    hex::encode(&envelope.msg_id),
                    e
                );
            }
        }
    }

    Ok(json!(messages))
}

/// `getmessagestatus` RPC implementation.
pub async fn rpc_get_message_status(
    params: &Value,
    relay_store: &Arc<RelayStore>,
    peer_registry: &Arc<PeerConnectionRegistry>,
) -> Result<Value, String> {
    let msg_id_str = params
        .get("msg_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'msg_id' parameter")?;

    let msg_id_bytes = hex::decode(msg_id_str).map_err(|_| "Invalid msg_id hex".to_string())?;
    if msg_id_bytes.len() != 32 {
        return Err("msg_id must be 32 bytes".to_string());
    }
    let mut msg_id = [0u8; 32];
    msg_id.copy_from_slice(&msg_id_bytes);

    // Check local status cache (cloned so we can use it after the move below)
    let cached_status = relay_store.get_status(&msg_id);
    let cached_status_for_upgrade = relay_store.get_status(&msg_id);

    // Query relay peers for fresh ack/delivery data
    peer_registry
        .broadcast(NetworkMessage::MsgAckQuery { msg_id })
        .await;

    // Give peers a moment to respond (existing acks in pending_relay_acks)
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Check for updated delivery data in local store
    let delivery = relay_store.get_delivery(&msg_id);
    let ack = relay_store.get_ack(&msg_id);

    let status = if ack.is_some() {
        MessageStatus::Read
    } else if delivery.is_some() {
        MessageStatus::Delivered
    } else {
        cached_status.unwrap_or(MessageStatus::Pending)
    };

    // Upgrade cached status if improved
    if let Some(ref d) = delivery {
        if !matches!(
            cached_status_for_upgrade,
            Some(MessageStatus::Read) | Some(MessageStatus::Delivered)
        ) {
            let _ = relay_store.set_status(&msg_id, &MessageStatus::Delivered);
        }
        let _ = d; // used above
    }

    let mut result = json!({
        "msg_id": msg_id_str,
        "status": status.as_str(),
    });
    if let Some(d) = delivery {
        result["delivered_at"] = json!(d.delivered_at);
    }
    if let Some(a) = ack {
        result["read_at"] = json!(a.read_at);
    }
    Ok(result)
}

async fn send_read_ack(
    envelope: &crate::messaging::types::TimeEnvelope,
    sender_addr: &str,
    recipient_key: &SigningKey,
    peer_registry: &Arc<PeerConnectionRegistry>,
) -> Result<(), MessageError> {
    use ed25519_dalek::Signer;
    let read_at = chrono::Utc::now().timestamp();
    let sig_bytes =
        crate::messaging::types::ReadAck::signing_bytes(&envelope.msg_id, sender_addr, read_at);
    let ack = crate::messaging::types::ReadAck {
        version: MSG_VERSION,
        msg_id: envelope.msg_id,
        sender_addr: sender_addr.to_string(),
        read_at,
        recipient_sig: recipient_key.sign(&sig_bytes).to_bytes(),
    };
    if let Ok(ack_bytes) = serde_cbor::to_vec(&ack) {
        peer_registry
            .broadcast(NetworkMessage::MsgReadAck {
                ack: ack_bytes,
                recipient_pubkey: recipient_key.verifying_key().to_bytes(),
            })
            .await;
    }
    Ok(())
}
