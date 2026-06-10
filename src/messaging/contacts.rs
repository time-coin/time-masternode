use crate::messaging::types::MessageError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub pubkey: [u8; 32],
    pub label: Option<String>,
    pub added_at: i64,
}

pub struct ContactsBook {
    tree: sled::Tree,
}

impl ContactsBook {
    pub fn open(db: &sled::Db) -> Result<Self, MessageError> {
        let tree = db
            .open_tree("contacts")
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(Self { tree })
    }

    /// In-memory fallback: used when the on-disk contacts DB cannot be opened.
    /// Contact data is not persisted across restarts but pubkey resolution still works.
    pub fn open_in_memory() -> Self {
        let db = sled::Config::new()
            .temporary(true)
            .open()
            .expect("ephemeral sled DB for ContactsBook");
        let tree = db.open_tree("contacts").expect("open contacts tree");
        Self { tree }
    }

    pub fn get(&self, address: &str) -> Option<Contact> {
        let bytes = self.tree.get(address.as_bytes()).ok()??;
        serde_cbor::from_slice(&bytes).ok()
    }

    pub fn upsert(&self, address: &str, contact: Contact) -> Result<(), MessageError> {
        let bytes =
            serde_cbor::to_vec(&contact).map_err(|e| MessageError::Storage(e.to_string()))?;
        self.tree
            .insert(address.as_bytes(), bytes)
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<(String, Contact)> {
        self.tree
            .iter()
            .filter_map(|r| r.ok())
            .filter_map(|(key, val)| {
                let addr = String::from_utf8(key.to_vec()).ok()?;
                let contact: Contact = serde_cbor::from_slice(&val).ok()?;
                Some((addr, contact))
            })
            .collect()
    }

    pub fn remove(&self, address: &str) -> Result<(), MessageError> {
        self.tree
            .remove(address.as_bytes())
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Look up a pubkey by SHA-256(address string) hash.
    /// Performs an O(n) scan of all contacts; used as a fallback when the
    /// utxo_manager does not have the address registered locally.
    pub fn get_pubkey_by_address_hash(&self, hash: &[u8; 32]) -> Option<[u8; 32]> {
        use sha2::Digest;
        self.tree.iter().find_map(|r| {
            let (key, val) = r.ok()?;
            let addr = std::str::from_utf8(&key).ok()?;
            let h: [u8; 32] = sha2::Sha256::digest(addr.as_bytes()).into();
            if &h == hash {
                let contact: Contact = serde_cbor::from_slice(&val).ok()?;
                Some(contact.pubkey)
            } else {
                None
            }
        })
    }
}
