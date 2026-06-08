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
}
