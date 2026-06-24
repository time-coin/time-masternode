use crate::messaging::types::{
    MessageError, TimeEnvelope, TimeMessage, MAX_TTL_SECONDS, MSG_VERSION,
};
use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Digest as _;
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey, StaticSecret};

/// Convert an Ed25519 public key to its X25519 Montgomery equivalent.
pub fn ed25519_pubkey_to_x25519(ed_pubkey: &[u8; 32]) -> [u8; 32] {
    let point = curve25519_dalek::edwards::CompressedEdwardsY(*ed_pubkey)
        .decompress()
        .unwrap_or_else(|| {
            // Fallback: use SHA-256 hash (valid key bytes should never reach here)
            let hash: [u8; 32] = sha2::Sha256::digest(ed_pubkey.as_slice()).into();
            let fallback = curve25519_dalek::edwards::CompressedEdwardsY(hash);
            fallback.decompress().unwrap_or_default()
        });
    point.to_montgomery().to_bytes()
}

/// Derive an X25519 static secret from an Ed25519 signing key.
/// Uses the same derivation as the Ed25519 scalar: SHA-512(seed)[0..32] with clamping.
pub fn ed25519_privkey_to_x25519(ed_privkey: &SigningKey) -> StaticSecret {
    use sha2::Digest;
    let hash = sha2::Sha512::digest(ed_privkey.to_bytes());
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&hash[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    StaticSecret::from(scalar)
}

/// Encrypt a plaintext TimeMessage into a TimeEnvelope.
pub fn encrypt_message(
    sender_key: &SigningKey,
    recipient_pubkey: &[u8; 32],
    plaintext: &[u8],
    recipient_addr: &str,
    ttl_seconds: u32,
    flags: u8,
) -> Result<TimeEnvelope, MessageError> {
    use rand::RngCore;
    use sha2::Digest;

    // 1. Ephemeral X25519 keypair
    let ephemeral_secret = EphemeralSecret::random_from_rng(rand::thread_rng());
    let ephemeral_pubkey = X25519PublicKey::from(&ephemeral_secret);

    // 2. Recipient X25519 pubkey
    let recipient_x25519 = X25519PublicKey::from(ed25519_pubkey_to_x25519(recipient_pubkey));

    // 3. ECDH shared secret
    let shared = ephemeral_secret.diffie_hellman(&recipient_x25519);

    // 4. Random nonce (24 bytes for XChaCha20)
    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    // 5. HKDF-SHA256: salt = nonce, info = "time-msg-v1"
    let hk = Hkdf::<Sha256>::new(Some(&nonce_bytes), shared.as_bytes());
    let mut sym_key = [0u8; 32];
    hk.expand(b"time-msg-v1", &mut sym_key)
        .map_err(|e| MessageError::Encryption(e.to_string()))?;

    // 6. Encrypt with XChaCha20-Poly1305
    let cipher = XChaCha20Poly1305::new_from_slice(&sym_key)
        .map_err(|e| MessageError::Encryption(e.to_string()))?;
    let nonce = chacha20poly1305::XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| MessageError::Encryption(e.to_string()))?;

    // 7. msg_id = SHA-256(ciphertext)
    let msg_id: [u8; 32] = sha2::Sha256::digest(&ciphertext).into();

    // 8. recipient_addr_hash = SHA-256(address bytes)
    let recipient_addr_hash: [u8; 32] = sha2::Sha256::digest(recipient_addr.as_bytes()).into();

    // 9. Sign: msg_id || recipient_addr_hash || nonce || ciphertext
    let mut sig_input = Vec::with_capacity(32 + 32 + 24 + ciphertext.len());
    sig_input.extend_from_slice(&msg_id);
    sig_input.extend_from_slice(&recipient_addr_hash);
    sig_input.extend_from_slice(&nonce_bytes);
    sig_input.extend_from_slice(&ciphertext);
    let sig: [u8; 64] = sender_key.sign(&sig_input).to_bytes();

    Ok(TimeEnvelope {
        version: MSG_VERSION,
        msg_id,
        recipient_addr_hash,
        sender_pubkey: sender_key.verifying_key().to_bytes(),
        ephemeral_pubkey: ephemeral_pubkey.to_bytes(),
        nonce: nonce_bytes,
        ciphertext_payload: ciphertext,
        sender_sig: sig,
        created_at: chrono::Utc::now().timestamp(),
        ttl_seconds: ttl_seconds.min(MAX_TTL_SECONDS),
        flags,
    })
}

/// Decrypt a TimeEnvelope using the recipient's Ed25519 private key.
/// Verifies sender signature before decrypting.
pub fn decrypt_envelope(
    recipient_key: &SigningKey,
    envelope: &TimeEnvelope,
) -> Result<TimeMessage, MessageError> {
    use sha2::Digest;

    // 1. Verify sender signature
    let sender_vk = VerifyingKey::from_bytes(&envelope.sender_pubkey)
        .map_err(|_| MessageError::InvalidSignature)?;
    let mut sig_input = Vec::new();
    sig_input.extend_from_slice(&envelope.msg_id);
    sig_input.extend_from_slice(&envelope.recipient_addr_hash);
    sig_input.extend_from_slice(&envelope.nonce);
    sig_input.extend_from_slice(&envelope.ciphertext_payload);
    let sig = ed25519_dalek::Signature::from_bytes(&envelope.sender_sig);
    sender_vk
        .verify(&sig_input, &sig)
        .map_err(|_| MessageError::InvalidSignature)?;

    // 2. Verify msg_id integrity
    let computed_id: [u8; 32] = sha2::Sha256::digest(&envelope.ciphertext_payload).into();
    if computed_id != envelope.msg_id {
        return Err(MessageError::InvalidSignature);
    }

    // 3. Recipient X25519 static secret
    let recipient_x25519 = ed25519_privkey_to_x25519(recipient_key);

    // 4. ECDH with ephemeral pubkey
    let ephemeral_pub = X25519PublicKey::from(envelope.ephemeral_pubkey);
    let shared = recipient_x25519.diffie_hellman(&ephemeral_pub);

    // 5. Derive symmetric key via HKDF-SHA256
    let hk = Hkdf::<Sha256>::new(Some(&envelope.nonce), shared.as_bytes());
    let mut sym_key = [0u8; 32];
    hk.expand(b"time-msg-v1", &mut sym_key)
        .map_err(|e| MessageError::Decryption(e.to_string()))?;

    // 6. Decrypt
    let cipher = XChaCha20Poly1305::new_from_slice(&sym_key)
        .map_err(|e| MessageError::Decryption(e.to_string()))?;
    let nonce = chacha20poly1305::XNonce::from_slice(&envelope.nonce);
    let plaintext = cipher
        .decrypt(nonce, envelope.ciphertext_payload.as_ref())
        .map_err(|e| MessageError::Decryption(e.to_string()))?;

    // 7. Deserialise CBOR plaintext
    serde_cbor::from_slice(&plaintext).map_err(|e| MessageError::Serialisation(e.to_string()))
}
