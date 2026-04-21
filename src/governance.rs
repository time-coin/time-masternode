//! On-chain governance for TIME Coin.
//!
//! ## Proposal lifecycle
//!
//! 1. Any Bronze/Silver/Gold masternode submits a proposal via the `submitproposal` RPC.
//! 2. The proposal is stored on-chain and gossiped to all peers.
//! 3. Masternodes vote YES or NO via the `voteproposal` RPC during the voting window
//!    (1008 blocks ≈ 1 week at 10 min/block).
//! 4. At the block where `vote_end_height` is reached, `check_and_execute_proposals()`
//!    tallies votes. If YES weight ≥ 67% of total active governance weight, the proposal
//!    passes and is executed immediately.
//!
//! ## Supported proposal types
//!
//! - `TreasurySpend`: disburse satoshis from the treasury to a recipient address.
//! - `FeeScheduleChange`: replace the active fee schedule (min fee + tiered rates).
//!
//! ## Sled key scheme
//!
//! | Key | Value |
//! |-----|-------|
//! | `gov_proposal_{64-char hex id}` | `bincode(GovernanceProposal)` |
//! | `gov_vote_{64-char hex id}_{voter_address}` | `bincode(GovernanceVote)` |

use crate::types::{Hash256, MasternodeTier, Signature};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum governance voting weight per masternode, regardless of tier.
/// Caps Gold nodes (100) at Silver level (10) so no single operator can dominate
/// governance by running many Gold masternodes.
const MAX_GOVERNANCE_VOTE_WEIGHT: u64 = 10;
use tokio::sync::RwLock;

/// Custom serde module for `[u8; 64]` Ed25519 signatures.
/// serde's built-in array support only covers arrays up to [u8; 32].
mod sig_serde {
    use serde::{de::SeqAccess, de::Visitor, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(sig: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut seq = s.serialize_tuple(64)?;
        for byte in sig.iter() {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        struct SigVisitor;
        impl<'de> Visitor<'de> for SigVisitor {
            type Value = [u8; 64];
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "an Ed25519 signature (64 bytes)")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<[u8; 64], A::Error> {
                let mut arr = [0u8; 64];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(arr)
            }
        }
        d.deserialize_tuple(64, SigVisitor)
    }
}

/// Blocks in the voting window (~1 week at 10 min/block).
pub const VOTING_PERIOD_BLOCKS: u64 = 1008;

/// Quorum numerator: YES weight must be ≥ QUORUM_NUMERATOR / QUORUM_DENOMINATOR of total weight.
const QUORUM_NUMERATOR: u64 = 67;
const QUORUM_DENOMINATOR: u64 = 100;

/// Max description length (bytes).
const MAX_DESCRIPTION_LEN: usize = 256;

// ── Proposal payload ──────────────────────────────────────────────────────────

/// What a governance proposal requests.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ProposalPayload {
    /// Disburse `amount` satoshis from the treasury to `recipient`.
    TreasurySpend {
        recipient: String,
        amount: u64,
        description: String,
    },
    /// Replace the active fee schedule.
    /// `tiers`: ordered vec of `(upper_bound_satoshis, rate_basis_points)`.
    FeeScheduleChange {
        new_min_fee: u64,
        new_tiers: Vec<(u64, u64)>,
    },
    /// Adjust the per-block emission rate.
    ///
    /// `new_satoshis_per_block` must be in the range [10 TIME, 10,000 TIME]
    /// (1_000_000_000 – 1_000_000_000_000 satoshis). The change takes effect on
    /// the block after the proposal is executed. The 30/5/65 internal split
    /// proportions remain fixed; only the total pie size changes.
    EmissionRateChange {
        new_satoshis_per_block: u64,
        description: String,
    },
}

// ── Proposal status ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ProposalStatus {
    Active,
    Passed { execute_at_height: u64 },
    Failed,
    Executed,
}

// ── GovernanceProposal ────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceProposal {
    /// SHA-256(bincode(payload) || submitter_pubkey || submit_height.le_bytes)
    pub id: Hash256,
    pub payload: ProposalPayload,
    /// Masternode network address of the submitter (e.g. "1.2.3.4:24000").
    pub submitter_address: String,
    /// Ed25519 verifying key of the submitter (32 bytes).
    pub submitter_pubkey: [u8; 32],
    /// Ed25519 signature over `id` by `submitter_pubkey`.
    #[serde(with = "sig_serde")]
    pub submitter_signature: Signature,
    pub submit_height: u64,
    /// submit_height + VOTING_PERIOD_BLOCKS
    pub vote_end_height: u64,
    pub status: ProposalStatus,
}

impl GovernanceProposal {
    /// Canonical proposal ID.
    pub fn compute_id(
        payload_bytes: &[u8],
        submitter_pubkey: &[u8; 32],
        submit_height: u64,
    ) -> Hash256 {
        let mut h = Sha256::new();
        h.update(payload_bytes);
        h.update(submitter_pubkey);
        h.update(submit_height.to_le_bytes());
        h.finalize().into()
    }

    /// Verify the submitter's signature over `self.id`.
    pub fn verify_signature(&self) -> Result<(), String> {
        let vk = VerifyingKey::from_bytes(&self.submitter_pubkey)
            .map_err(|e| format!("Bad submitter pubkey: {e}"))?;
        let sig = ed25519_dalek::Signature::from_bytes(&self.submitter_signature);
        vk.verify(&self.id, &sig)
            .map_err(|_| "Invalid proposal signature".to_string())
    }

    /// Sign this proposal with the given key, setting `submitter_signature`.
    pub fn sign(&mut self, key: &SigningKey) {
        self.submitter_signature = key.sign(&self.id).to_bytes();
    }
}

// ── GovernanceVote ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceVote {
    pub proposal_id: Hash256,
    /// Masternode network address of the voter.
    pub voter_address: String,
    /// Ed25519 verifying key of the voter (32 bytes).
    pub voter_pubkey: [u8; 32],
    /// true = YES, false = NO
    pub approve: bool,
    pub vote_height: u64,
    /// Ed25519 signature over `signing_message()`.
    #[serde(with = "sig_serde")]
    pub signature: Signature,
}

impl GovernanceVote {
    /// Bytes that must be signed. Deterministic and injective.
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = Vec::with_capacity(32 + self.voter_address.len() + 1 + 8);
        msg.extend_from_slice(&self.proposal_id);
        msg.extend_from_slice(self.voter_address.as_bytes());
        msg.push(u8::from(self.approve));
        msg.extend_from_slice(&self.vote_height.to_le_bytes());
        msg
    }

    /// Sign this vote with the given key.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signature = key.sign(&self.signing_message()).to_bytes();
    }

    /// Verify the signature.
    pub fn verify_signature(&self) -> Result<(), String> {
        let vk = VerifyingKey::from_bytes(&self.voter_pubkey)
            .map_err(|e| format!("Bad voter pubkey: {e}"))?;
        let sig = ed25519_dalek::Signature::from_bytes(&self.signature);
        vk.verify(&self.signing_message(), &sig)
            .map_err(|_| "Invalid vote signature".to_string())
    }
}

// ── GovernanceState ───────────────────────────────────────────────────────────

/// The governance subsystem. Holds sled-backed storage and in-memory indexes.
pub struct GovernanceState {
    db: sled::Db,
    /// proposal_id_hex → GovernanceProposal
    proposals: Arc<RwLock<HashMap<String, GovernanceProposal>>>,
    /// proposal_id_hex → (voter_address → GovernanceVote)
    votes: Arc<RwLock<HashMap<String, HashMap<String, GovernanceVote>>>>,
}

impl GovernanceState {
    /// Create and load from storage.
    pub fn new(db: sled::Db) -> Result<Self, String> {
        let gs = Self {
            db,
            proposals: Arc::new(RwLock::new(HashMap::new())),
            votes: Arc::new(RwLock::new(HashMap::new())),
        };
        // Load synchronously via a blocking call (called once at startup).
        gs.load_from_storage()?;
        Ok(gs)
    }

    fn load_from_storage(&self) -> Result<(), String> {
        // Load proposals
        let props: Vec<GovernanceProposal> = self
            .db
            .scan_prefix(b"gov_proposal_")
            .filter_map(|r| r.ok())
            .filter_map(|(_, v)| bincode::deserialize::<GovernanceProposal>(&v).ok())
            .collect();

        // Load votes
        let all_votes: Vec<GovernanceVote> = self
            .db
            .scan_prefix(b"gov_vote_")
            .filter_map(|r| r.ok())
            .filter_map(|(_, v)| bincode::deserialize::<GovernanceVote>(&v).ok())
            .collect();

        // This runs at startup before the tokio runtime is fully active, so we
        // use try_write() with a brief spin rather than blocking_write().
        let mut prop_map = self
            .proposals
            .try_write()
            .map_err(|_| "governance proposal lock poisoned at load")?;
        for p in props {
            prop_map.insert(hex::encode(p.id), p);
        }
        drop(prop_map);

        let mut vote_map = self
            .votes
            .try_write()
            .map_err(|_| "governance vote lock poisoned at load")?;
        for v in all_votes {
            vote_map
                .entry(hex::encode(v.proposal_id))
                .or_default()
                .insert(v.voter_address.clone(), v);
        }

        Ok(())
    }

    // ── Persistence helpers ───────────────────────────────────────────────────

    fn persist_proposal(&self, proposal: &GovernanceProposal) -> Result<(), String> {
        let key = format!("gov_proposal_{}", hex::encode(proposal.id));
        let val = bincode::serialize(proposal).map_err(|e| format!("serialize proposal: {e}"))?;
        self.db
            .insert(key.as_bytes(), val)
            .map_err(|e| format!("sled insert proposal: {e}"))?;
        Ok(())
    }

    fn persist_vote(&self, vote: &GovernanceVote) -> Result<(), String> {
        let key = format!(
            "gov_vote_{}_{}",
            hex::encode(vote.proposal_id),
            vote.voter_address
        );
        let val = bincode::serialize(vote).map_err(|e| format!("serialize vote: {e}"))?;
        self.db
            .insert(key.as_bytes(), val)
            .map_err(|e| format!("sled insert vote: {e}"))?;
        Ok(())
    }

    // ── Submission ────────────────────────────────────────────────────────────

    /// Validate and store an incoming proposal.
    pub async fn submit_proposal(
        &self,
        proposal: GovernanceProposal,
        registry: &crate::masternode_registry::MasternodeRegistry,
        treasury_balance: u64,
    ) -> Result<(), String> {
        let id_hex = hex::encode(proposal.id);

        // Idempotent: already known
        if self.proposals.read().await.contains_key(&id_hex) {
            return Ok(());
        }

        // Submitter must be an active Bronze/Silver/Gold masternode
        let active = registry.get_active_masternodes().await;
        let submitter = active
            .iter()
            .find(|mn| mn.masternode.address == proposal.submitter_address)
            .ok_or_else(|| {
                format!(
                    "Submitter {} is not an active masternode",
                    proposal.submitter_address
                )
            })?;

        if submitter.masternode.tier == MasternodeTier::Free {
            return Err("Free-tier masternodes cannot submit governance proposals".to_string());
        }

        // Signature
        proposal.verify_signature()?;

        // Payload validation
        match &proposal.payload {
            ProposalPayload::TreasurySpend {
                amount,
                description,
                recipient,
            } => {
                if *amount == 0 {
                    return Err("TreasurySpend amount must be > 0".to_string());
                }
                if *amount > treasury_balance {
                    return Err(format!(
                        "TreasurySpend amount {amount} exceeds treasury balance {treasury_balance}"
                    ));
                }
                if description.len() > MAX_DESCRIPTION_LEN {
                    return Err(format!(
                        "Description too long ({} bytes, max {MAX_DESCRIPTION_LEN})",
                        description.len()
                    ));
                }
                if recipient.is_empty() {
                    return Err("Recipient address is empty".to_string());
                }
            }
            ProposalPayload::FeeScheduleChange {
                new_min_fee,
                new_tiers,
            } => {
                if *new_min_fee == 0 {
                    return Err("new_min_fee must be > 0".to_string());
                }
                if new_tiers.is_empty() {
                    return Err("new_tiers must not be empty".to_string());
                }
                // Tiers must be ordered ascending by upper bound
                for w in new_tiers.windows(2) {
                    if w[0].0 >= w[1].0 {
                        return Err(
                            "fee tiers must be ordered ascending by upper_bound".to_string()
                        );
                    }
                }
            }
            ProposalPayload::EmissionRateChange {
                new_satoshis_per_block,
                description,
            } => {
                const MIN_REWARD: u64 = 10 * 100_000_000; // 10 TIME
                const MAX_REWARD: u64 = 10_000 * 100_000_000; // 10,000 TIME
                if *new_satoshis_per_block < MIN_REWARD || *new_satoshis_per_block > MAX_REWARD {
                    return Err(format!(
                        "new_satoshis_per_block {new_satoshis_per_block} outside allowed range [{MIN_REWARD}, {MAX_REWARD}]"
                    ));
                }
                if description.len() > MAX_DESCRIPTION_LEN {
                    return Err(format!(
                        "Description too long ({} bytes, max {MAX_DESCRIPTION_LEN})",
                        description.len()
                    ));
                }
            }
        }

        // Persist and index
        self.persist_proposal(&proposal)?;
        self.proposals.write().await.insert(id_hex, proposal);

        Ok(())
    }

    // ── Voting ────────────────────────────────────────────────────────────────

    /// Record a vote. Returns Ok(true) if this was a new/changed vote, Ok(false) if duplicate.
    pub async fn record_vote(
        &self,
        vote: GovernanceVote,
        registry: &crate::masternode_registry::MasternodeRegistry,
    ) -> Result<bool, String> {
        let prop_id_hex = hex::encode(vote.proposal_id);

        // Proposal must exist and be Active
        {
            let props = self.proposals.read().await;
            let proposal = props
                .get(&prop_id_hex)
                .ok_or_else(|| format!("Proposal {} not found", prop_id_hex))?;
            if proposal.status != ProposalStatus::Active {
                return Err(format!("Proposal {prop_id_hex} is not active"));
            }
            if vote.vote_height > proposal.vote_end_height {
                return Err("Vote submitted after voting period ended".to_string());
            }
        }

        // Voter must be an active masternode that can vote (tier >= Bronze)
        let active = registry.get_active_masternodes().await;
        let voter_mn = active
            .iter()
            .find(|mn| mn.masternode.address == vote.voter_address)
            .ok_or_else(|| format!("Voter {} is not an active masternode", vote.voter_address))?;

        if voter_mn.masternode.tier == MasternodeTier::Free {
            return Err("Free-tier masternodes cannot vote on governance proposals".to_string());
        }

        // Signature
        vote.verify_signature()?;

        // Check if this is a new or duplicate vote
        let existing_approve = self
            .votes
            .read()
            .await
            .get(&prop_id_hex)
            .and_then(|m| m.get(&vote.voter_address))
            .map(|v| v.approve);

        let is_new = existing_approve != Some(vote.approve);

        if is_new {
            self.persist_vote(&vote)?;
            self.votes
                .write()
                .await
                .entry(prop_id_hex)
                .or_default()
                .insert(vote.voter_address.clone(), vote);
        }

        Ok(is_new)
    }

    // ── Tally & execution ─────────────────────────────────────────────────────

    /// Called once per block by `Blockchain::add_block()`.
    /// Returns proposals that passed and should be executed.
    pub async fn check_and_execute_proposals(
        &self,
        height: u64,
        registry: &crate::masternode_registry::MasternodeRegistry,
    ) -> Vec<GovernanceProposal> {
        let total_weight = Self::total_active_weight(registry).await;

        // Find Active proposals whose voting window just closed
        let maturing: Vec<String> = self
            .proposals
            .read()
            .await
            .iter()
            .filter(|(_, p)| p.status == ProposalStatus::Active && p.vote_end_height == height)
            .map(|(id, _)| id.clone())
            .collect();

        let mut passed = Vec::new();

        for id_hex in maturing {
            let yes_weight = self.tally_yes_weight(&id_hex, registry).await;

            let quorum_met = total_weight > 0
                && yes_weight * QUORUM_DENOMINATOR >= total_weight * QUORUM_NUMERATOR;

            let mut props = self.proposals.write().await;
            if let Some(proposal) = props.get_mut(&id_hex) {
                if quorum_met {
                    proposal.status = ProposalStatus::Passed {
                        execute_at_height: height,
                    };
                    tracing::info!(
                        "🗳️  Governance proposal {} PASSED (yes={yes_weight}, total={total_weight})",
                        &id_hex[..12]
                    );
                    passed.push(proposal.clone());
                } else {
                    proposal.status = ProposalStatus::Failed;
                    tracing::info!(
                        "🗳️  Governance proposal {} FAILED (yes={yes_weight}, total={total_weight})",
                        &id_hex[..12]
                    );
                }
                if let Err(e) = self.persist_proposal(proposal) {
                    tracing::error!("Failed to persist proposal status update: {e}");
                }
            }
        }

        passed
    }

    /// Mark a proposal as Executed after `Blockchain` has applied it.
    pub async fn mark_executed(&self, proposal_id: &Hash256) {
        let id_hex = hex::encode(proposal_id);
        let mut props = self.proposals.write().await;
        if let Some(p) = props.get_mut(&id_hex) {
            p.status = ProposalStatus::Executed;
            if let Err(e) = self.persist_proposal(p) {
                tracing::error!("Failed to persist Executed status: {e}");
            }
        }
    }

    async fn tally_yes_weight(
        &self,
        prop_id_hex: &str,
        registry: &crate::masternode_registry::MasternodeRegistry,
    ) -> u64 {
        let active = registry.get_active_masternodes().await;
        let addr_to_weight: HashMap<String, u64> = active
            .iter()
            .map(|mn| {
                (
                    mn.masternode.address.clone(),
                    mn.masternode
                        .tier
                        .voting_power()
                        .0
                        .min(MAX_GOVERNANCE_VOTE_WEIGHT),
                )
            })
            .collect();

        let votes = self.votes.read().await;
        let prop_votes = match votes.get(prop_id_hex) {
            Some(m) => m,
            None => return 0,
        };

        prop_votes
            .values()
            .filter(|v| v.approve)
            .map(|v| addr_to_weight.get(&v.voter_address).copied().unwrap_or(0))
            .sum()
    }

    async fn total_active_weight(registry: &crate::masternode_registry::MasternodeRegistry) -> u64 {
        registry
            .get_active_masternodes()
            .await
            .iter()
            .map(|mn| {
                mn.masternode
                    .tier
                    .voting_power()
                    .0
                    .min(MAX_GOVERNANCE_VOTE_WEIGHT)
            })
            .sum()
    }

    // ── Deterministic pre-commit query ───────────────────────────────────────

    /// Returns the total TreasurySpend satoshis that will be disbursed when
    /// `height` is reached (i.e. proposals with `vote_end_height == height`
    /// that currently have enough YES weight to pass quorum).
    ///
    /// This is a pure read — no state is mutated.  The block producer calls
    /// this before sealing the block so that `BlockHeader.treasury_balance`
    /// commits the post-governance balance, making it a complete, auditable
    /// snapshot that never overstates the treasury.
    pub async fn treasury_spends_maturing_at(
        &self,
        height: u64,
        registry: &crate::masternode_registry::MasternodeRegistry,
    ) -> u64 {
        let total_weight = Self::total_active_weight(registry).await;

        let maturing_ids: Vec<String> = self
            .proposals
            .read()
            .await
            .iter()
            .filter(|(_, p)| p.status == ProposalStatus::Active && p.vote_end_height == height)
            .map(|(id, _)| id.clone())
            .collect();

        let mut total_spend = 0u64;
        for id_hex in &maturing_ids {
            let yes_weight = self.tally_yes_weight(id_hex, registry).await;
            let quorum_met = total_weight > 0
                && yes_weight * QUORUM_DENOMINATOR >= total_weight * QUORUM_NUMERATOR;
            if !quorum_met {
                continue;
            }
            let props = self.proposals.read().await;
            if let Some(proposal) = props.get(id_hex) {
                if let ProposalPayload::TreasurySpend { amount, .. } = &proposal.payload {
                    total_spend = total_spend.saturating_add(*amount);
                }
            }
        }
        total_spend
    }

    // ── Query helpers (RPC) ───────────────────────────────────────────────────

    pub async fn list_proposals(&self) -> Vec<GovernanceProposal> {
        self.proposals.read().await.values().cloned().collect()
    }

    pub async fn get_proposal(&self, id: &Hash256) -> Option<GovernanceProposal> {
        self.proposals.read().await.get(&hex::encode(id)).cloned()
    }

    pub async fn get_votes_for(&self, proposal_id: &Hash256) -> Vec<GovernanceVote> {
        self.votes
            .read()
            .await
            .get(&hex::encode(proposal_id))
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Current YES weight for a proposal (for RPC display).
    pub async fn yes_weight(
        &self,
        proposal_id: &Hash256,
        registry: &crate::masternode_registry::MasternodeRegistry,
    ) -> u64 {
        self.tally_yes_weight(&hex::encode(proposal_id), registry)
            .await
    }

    /// Total active governance weight (for RPC display).
    pub async fn total_weight(registry: &crate::masternode_registry::MasternodeRegistry) -> u64 {
        Self::total_active_weight(registry).await
    }
}
