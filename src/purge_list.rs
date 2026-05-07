//! Hardcoded banlist of finalized transactions that must be evicted from
//! every node's state.
//!
//! Some early-network bugs allowed transactions to TimeVote-finalize despite
//! referencing inputs that did not exist on-chain.  After the validator was
//! hardened, these transactions can never be included in a block — they are
//! permanently stuck in the finalized pool, blocking block production for
//! the producers that hold them and creating a divergent UTXO view (phantom
//! outputs visible only on the holder's node).
//!
//! This module captures, as static data, the txid plus the structural facts
//! needed to undo the effect locally on every node:
//!
//! - `real_inputs` — outpoints that actually exist on-chain and were locked
//!   by the bad TX.  These get restored from `SpentFinalized` → `Unspent`.
//! - `phantom_output_vouts` — outputs that the bad TX synthesized.  These
//!   get removed from the local UTXO set.
//!
//! The purge runs on every startup until the banlist is removed in a
//! later release.  Once the network has converged this module can be deleted.

use crate::types::{Hash256, OutPoint};

pub struct PhantomTxRecord {
    pub txid_hex: &'static str,
    pub real_inputs: &'static [(&'static str, u32)],
    pub phantom_output_vouts: &'static [u32],
    pub reason: &'static str,
}

/// Finalized-pool banlist.  See module docs.
pub const PHANTOM_FINALIZED_TXS: &[PhantomTxRecord] = &[PhantomTxRecord {
    txid_hex: "c19c230fe4b8e60545e1e4423710108fbc3d5fe1a15f4984bd99fc3bd12477a5",
    real_inputs: &[
        (
            "3a99ec9ec2576536bad0490a1c0ef943ffd6e774bec1c46dcc8b74a9df599817",
            0,
        ),
        (
            "b275010b92f69d3b1782f48b6b07d93fe25c6451f8d730a03ff84395da639c7c",
            0,
        ),
    ],
    phantom_output_vouts: &[0, 1],
    reason: "Pre-hardening phantom-input TX: third input \
             20b9365e...:0 never existed on-chain; minted ~99,900 TIME and \
             cannot be included in a block by post-fix validators.",
}];

fn parse_txid(hex_str: &str) -> Option<Hash256> {
    let bytes = hex::decode(hex_str).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

/// Returns true if the given txid is in the phantom-finalized banlist.
pub fn is_banned(txid: &Hash256) -> bool {
    PHANTOM_FINALIZED_TXS
        .iter()
        .any(|rec| parse_txid(rec.txid_hex).as_ref() == Some(txid))
}

/// Iterator over (txid, real_inputs as OutPoints, phantom outpoints to remove).
pub fn iter_records() -> impl Iterator<Item = (Hash256, Vec<OutPoint>, Vec<OutPoint>, &'static str)>
{
    PHANTOM_FINALIZED_TXS.iter().filter_map(|rec| {
        let txid = parse_txid(rec.txid_hex)?;
        let real_inputs = rec
            .real_inputs
            .iter()
            .filter_map(|(t, v)| {
                Some(OutPoint {
                    txid: parse_txid(t)?,
                    vout: *v,
                })
            })
            .collect();
        let phantom_outputs = rec
            .phantom_output_vouts
            .iter()
            .map(|v| OutPoint { txid, vout: *v })
            .collect();
        Some((txid, real_inputs, phantom_outputs, rec.reason))
    })
}
