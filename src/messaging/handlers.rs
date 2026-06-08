use crate::messaging::relay::RelayStore;
use crate::messaging::types::{MessageError, ReadAck, RelayStorageAck, TimeEnvelope};
use crate::network::message::NetworkMessage;
use crate::types::MasternodeTier;
use ed25519_dalek::SigningKey;
use std::sync::Arc;

/// Handle MsgSubmit — Silver/Gold nodes store; others drop.
pub async fn handle_msg_submit(
    envelope_bytes: &[u8],
    relay_store: Option<&Arc<RelayStore>>,
    node_tier: MasternodeTier,
    node_signing_key: &SigningKey,
) -> Result<Option<NetworkMessage>, String> {
    let store = match (node_tier, relay_store) {
        (MasternodeTier::Silver | MasternodeTier::Gold, Some(s)) => s,
        _ => {
            tracing::debug!("📨 MsgSubmit ignored — not a relay node");
            return Ok(None);
        }
    };

    let envelope = match TimeEnvelope::deserialise(envelope_bytes) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("📨 MsgSubmit: invalid envelope: {}", e);
            return Ok(None);
        }
    };

    if envelope.is_expired() {
        tracing::warn!("📨 MsgSubmit: envelope already expired, ignoring");
        return Ok(None);
    }

    match store.store_envelope(&envelope) {
        Ok(()) => {
            tracing::info!(
                "📨 Stored message {} for relay",
                hex::encode(&envelope.msg_id)
            );
            match RelayStore::build_storage_ack(&envelope, node_signing_key) {
                Ok(ack) => match serde_cbor::to_vec(&ack) {
                    Ok(ack_bytes) => Ok(Some(NetworkMessage::MsgRelayAck { ack: ack_bytes })),
                    Err(e) => {
                        tracing::warn!("📨 MsgSubmit: failed to serialise ack: {}", e);
                        Ok(None)
                    }
                },
                Err(e) => {
                    tracing::warn!("📨 MsgSubmit: failed to build ack: {}", e);
                    Ok(None)
                }
            }
        }
        Err(MessageError::TooLarge(got, max)) => {
            tracing::warn!(
                "📨 MsgSubmit: envelope too large ({} > {}), rejecting",
                got,
                max
            );
            Ok(None)
        }
        Err(e) => {
            tracing::warn!("📨 MsgSubmit: store failed: {}", e);
            Ok(None)
        }
    }
}

/// Handle MsgFetchPending — return all unexpired envelopes for this recipient.
pub async fn handle_msg_fetch_pending(
    recipient_addr_hash: &[u8; 32],
    since: i64,
    relay_store: Option<&Arc<RelayStore>>,
    node_signing_key: &SigningKey,
) -> Result<Option<NetworkMessage>, String> {
    let store = match relay_store {
        Some(s) => s,
        None => return Ok(None),
    };

    let pending = store.fetch_pending(recipient_addr_hash);
    let filtered: Vec<_> = pending.iter().filter(|e| e.created_at >= since).collect();

    for env in &filtered {
        let _ = store.record_delivery(&env.msg_id, node_signing_key);
    }

    let envelopes: Vec<Vec<u8>> = filtered.iter().filter_map(|e| e.serialise().ok()).collect();

    if envelopes.is_empty() {
        return Ok(None);
    }

    Ok(Some(NetworkMessage::MsgEnvelopes {
        recipient_addr_hash: *recipient_addr_hash,
        envelopes,
    }))
}

/// Handle MsgReadAck — validate and store the read receipt.
pub async fn handle_msg_read_ack(
    ack_bytes: &[u8],
    recipient_pubkey: &[u8; 32],
    relay_store: Option<&Arc<RelayStore>>,
) -> Result<Option<NetworkMessage>, String> {
    let store = match relay_store {
        Some(s) => s,
        None => return Ok(None),
    };

    let ack: ReadAck = match serde_cbor::from_slice(ack_bytes) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("📨 MsgReadAck: invalid ack bytes: {}", e);
            return Ok(None);
        }
    };

    match store.store_ack(&ack, recipient_pubkey) {
        Ok(()) => tracing::info!("✅ Stored ReadAck for {}", hex::encode(&ack.msg_id)),
        Err(e) => tracing::warn!("📨 MsgReadAck: store failed: {}", e),
    }
    Ok(None)
}

/// Handle MsgAckQuery — return ReadAck and/or DeliveryEvent for a msg_id.
pub async fn handle_msg_ack_query(
    msg_id: &[u8; 32],
    relay_store: Option<&Arc<RelayStore>>,
) -> Result<Option<NetworkMessage>, String> {
    let store = match relay_store {
        Some(s) => s,
        None => return Ok(None),
    };

    let ack = store
        .get_ack(msg_id)
        .and_then(|a| serde_cbor::to_vec(&a).ok());
    let delivery = store
        .get_delivery(msg_id)
        .and_then(|d| serde_cbor::to_vec(&d).ok());

    Ok(Some(NetworkMessage::MsgAckResponse {
        msg_id: *msg_id,
        ack,
        delivery,
    }))
}

/// Handle MsgPubkeyQuery — look up address pubkey from utxo_manager cache.
pub async fn handle_pubkey_query(
    address_hash: &[u8; 32],
    utxo_manager: &Arc<crate::utxo_manager::UTXOStateManager>,
) -> Result<Option<NetworkMessage>, String> {
    let pubkey = utxo_manager.get_pubkey_by_address_hash(address_hash);
    Ok(Some(NetworkMessage::MsgPubkeyResponse {
        address_hash: *address_hash,
        pubkey,
    }))
}

/// Handle MsgRelayAck — forward to the pending ack map in peer_registry.
pub fn handle_msg_relay_ack(
    ack_bytes: &[u8],
    peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
) -> Result<Option<NetworkMessage>, String> {
    let ack: RelayStorageAck = match serde_cbor::from_slice(ack_bytes) {
        Ok(a) => a,
        Err(e) => {
            tracing::debug!("📨 MsgRelayAck: could not parse ack: {}", e);
            return Ok(None);
        }
    };
    if let Some(tx) = peer_registry.pending_relay_acks.get(&ack.msg_id) {
        let _ = tx.send(ack_bytes.to_vec());
    }
    Ok(None)
}

/// Handle MsgEnvelopes — forward to the pending fetch map in peer_registry.
pub fn handle_msg_envelopes(
    recipient_addr_hash: &[u8; 32],
    envelopes: &[Vec<u8>],
    peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
) -> Result<Option<NetworkMessage>, String> {
    if let Some(tx) = peer_registry.pending_msg_envelopes.get(recipient_addr_hash) {
        for env_bytes in envelopes {
            let _ = tx.send(env_bytes.clone());
        }
    }
    Ok(None)
}

/// Handle MsgPubkeyResponse — forward to the pending pubkey query map in peer_registry.
pub fn handle_pubkey_response(
    address_hash: &[u8; 32],
    pubkey: Option<[u8; 32]>,
    peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
) -> Result<Option<NetworkMessage>, String> {
    if let Some(pk) = pubkey {
        if let Some(tx) = peer_registry.pending_pubkey_queries.get(address_hash) {
            let _ = tx.send(pk);
        }
    }
    Ok(None)
}
