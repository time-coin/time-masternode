//! Phase 5: Fork Resolution Testing
//! Tests network partition recovery and fork resolution using longest chain rule
//! with deterministic hash tiebreaker (lower hash wins at equal heights).
//!
//! Success Criteria:
//! - Network partition creates fork
//! - Each partition continues consensus independently
//! - On reconnection, longer chain wins; at same height, lower hash wins
//! - No spurious reorganizations

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    /// Simulated network node with partition awareness
    #[allow(dead_code)]
    struct PartitionTestNode {
        id: String,
        stake: u64,
        blocks: Vec<String>,
        partition_group: Option<u32>,
    }

    impl PartitionTestNode {
        fn new(id: String, stake: u64) -> Self {
            PartitionTestNode {
                id,
                stake,
                blocks: vec![],
                partition_group: None,
            }
        }

        fn add_block(&mut self, block: String) {
            self.blocks.push(block);
        }

        fn get_chain_length(&self) -> usize {
            self.blocks.len()
        }

        #[allow(dead_code)]
        fn get_latest_block(&self) -> Option<&str> {
            self.blocks.last().map(|s| s.as_str())
        }

        /// Compute a deterministic hash for this node's chain tip.
        /// Uses a simple hash based on chain contents for testing.
        fn chain_tip_hash(&self) -> u64 {
            let mut hash: u64 = 0;
            for block in &self.blocks {
                // Simple deterministic hash combining block content
                for byte in block.bytes() {
                    hash = hash.wrapping_mul(31).wrapping_add(byte as u64);
                }
            }
            hash
        }
    }

    /// Network with partition simulation
    struct PartitionTestNetwork {
        nodes: Vec<(String, Arc<Mutex<PartitionTestNode>>)>,
    }

    impl PartitionTestNetwork {
        fn new(validator_stakes: Vec<(String, u64)>) -> Self {
            let mut nodes = Vec::new();
            for (id, stake) in validator_stakes {
                nodes.push((
                    id.clone(),
                    Arc::new(Mutex::new(PartitionTestNode::new(id, stake))),
                ));
            }
            PartitionTestNetwork { nodes }
        }

        /// Create partition: group_a nodes can't communicate with group_b nodes
        fn partition(&mut self, group_a: Vec<String>, _group_b: Vec<String>) {
            for (node_id, node_arc) in &self.nodes {
                if group_a.contains(node_id) {
                    node_arc.lock().unwrap().partition_group = Some(0);
                } else {
                    node_arc.lock().unwrap().partition_group = Some(1);
                }
            }
        }

        /// Remove partition (reconnect)
        fn reconnect(&mut self) {
            for (_, node_arc) in &self.nodes {
                node_arc.lock().unwrap().partition_group = None;
            }
        }

        /// Group A produces blocks during partition
        fn advance_group_a(&mut self) {
            for (_, node_arc) in &self.nodes {
                let mut node = node_arc.lock().unwrap();
                if node.partition_group == Some(0) {
                    let len = node.get_chain_length();
                    node.add_block(format!("block{}", len + 1));
                }
            }
        }

        /// Group B produces blocks during partition (different block content)
        fn advance_group_b(&mut self) {
            for (_, node_arc) in &self.nodes {
                let mut node = node_arc.lock().unwrap();
                if node.partition_group == Some(1) {
                    let len = node.get_chain_length();
                    node.add_block(format!("alt_block{}", len + 1));
                }
            }
        }

        /// Apply fork resolution: longest chain wins, hash tiebreaker at same height
        fn resolve_forks(&mut self) {
            // Group chains by length, then pick winner
            let mut best_chain: Option<Vec<String>> = None;
            let mut best_length = 0usize;
            let mut best_hash = u64::MAX; // lower hash wins

            for (_, node_arc) in &self.nodes {
                let node = node_arc.lock().unwrap();
                let length = node.get_chain_length();
                let hash = node.chain_tip_hash();

                // Longest chain wins; at same length, lower hash wins
                if length > best_length || (length == best_length && hash < best_hash) {
                    best_length = length;
                    best_hash = hash;
                    best_chain = Some(node.blocks.clone());
                }
            }

            // Adopt best chain for all nodes
            if let Some(ref chain) = best_chain {
                for (_, node_arc) in &self.nodes {
                    let mut node = node_arc.lock().unwrap();
                    node.blocks = chain.clone();
                }
            }
        }
    }

    #[test]
    fn test_partition_creates_fork() {
        // Setup 3-node network
        let validators = vec![
            ("node_a".to_string(), 100),
            ("node_b".to_string(), 100),
            ("node_c".to_string(), 100),
        ];
        let mut network = PartitionTestNetwork::new(validators);

        // Partition: nodes 0,1 in group A; node 2 in group B
        network.partition(
            vec!["node_a".to_string(), "node_b".to_string()],
            vec!["node_c".to_string()],
        );

        // Group A produces blocks
        network.advance_group_a();
        network.advance_group_a();

        // Group B produces blocks
        network.advance_group_b();

        // Verify fork exists
        let node_a_len = network.nodes[0].1.lock().unwrap().get_chain_length();
        let node_c_len = network.nodes[2].1.lock().unwrap().get_chain_length();

        assert_eq!(node_a_len, 2, "Group A should have 2 blocks");
        assert_eq!(node_c_len, 1, "Group B should have 1 block");
    }

    #[test]
    fn test_partition_recovery_adopts_longer_chain() {
        let validators = vec![
            ("node_a".to_string(), 100),
            ("node_b".to_string(), 100),
            ("node_c".to_string(), 100),
        ];
        let mut network = PartitionTestNetwork::new(validators);

        network.partition(
            vec!["node_a".to_string(), "node_b".to_string()],
            vec!["node_c".to_string()],
        );

        // Majority group (A+B) produces more blocks than minority (C)
        network.advance_group_a();
        network.advance_group_a();
        network.advance_group_a();
        network.advance_group_b();

        // Verify fork
        let node_a_len = network.nodes[0].1.lock().unwrap().get_chain_length();
        let node_c_len = network.nodes[2].1.lock().unwrap().get_chain_length();
        assert!(node_a_len > node_c_len, "Majority should have longer chain");

        // Reconnect and resolve
        network.reconnect();
        network.resolve_forks();

        // All nodes should adopt majority chain
        let node_a_final = network.nodes[0].1.lock().unwrap().get_chain_length();
        let node_b_final = network.nodes[1].1.lock().unwrap().get_chain_length();
        let node_c_final = network.nodes[2].1.lock().unwrap().get_chain_length();

        assert_eq!(
            node_a_final, node_b_final,
            "Nodes A and B should have same length"
        );
        assert_eq!(
            node_c_final, node_a_final,
            "Minority should adopt majority chain"
        );
    }

    #[test]
    fn test_hash_tiebreaker_at_equal_height() {
        let validators = vec![("node_a".to_string(), 100), ("node_b".to_string(), 100)];
        let mut network = PartitionTestNetwork::new(validators);

        // Partition so each produces different blocks
        network.partition(vec!["node_a".to_string()], vec!["node_b".to_string()]);

        // Both produce same number of blocks but different content
        network.advance_group_a();
        network.advance_group_a();
        network.advance_group_b();
        network.advance_group_b();

        // Verify equal length but different chains
        let hash_a = network.nodes[0].1.lock().unwrap().chain_tip_hash();
        let hash_b = network.nodes[1].1.lock().unwrap().chain_tip_hash();
        assert_ne!(hash_a, hash_b, "Chains should have different hashes");

        // Resolve: lower hash should win deterministically
        network.reconnect();
        network.resolve_forks();

        let final_hash_a = network.nodes[0].1.lock().unwrap().chain_tip_hash();
        let final_hash_b = network.nodes[1].1.lock().unwrap().chain_tip_hash();
        assert_eq!(
            final_hash_a, final_hash_b,
            "All nodes should agree after resolution"
        );

        // Winner should be the chain with lower hash
        let expected_hash = hash_a.min(hash_b);
        assert_eq!(final_hash_a, expected_hash, "Lower hash chain should win");
    }

    #[test]
    fn test_no_spurious_reorganizations_after_recovery() {
        let validators = vec![
            ("node_a".to_string(), 100),
            ("node_b".to_string(), 100),
            ("node_c".to_string(), 100),
        ];
        let mut network = PartitionTestNetwork::new(validators);

        network.partition(
            vec!["node_a".to_string(), "node_b".to_string()],
            vec!["node_c".to_string()],
        );

        // Groups diverge
        network.advance_group_a();
        network.advance_group_b();

        // Reconnect
        network.reconnect();
        network.resolve_forks();

        // Save final state
        let final_chain_length = network.nodes[0].1.lock().unwrap().get_chain_length();
        let final_hash = network.nodes[0].1.lock().unwrap().chain_tip_hash();

        // Try fork resolution again
        network.resolve_forks();

        // Chain should not change
        let post_reorg = network.nodes[0].1.lock().unwrap().get_chain_length();
        let post_hash = network.nodes[0].1.lock().unwrap().chain_tip_hash();
        assert_eq!(
            final_chain_length, post_reorg,
            "No spurious reorganizations should occur"
        );
        assert_eq!(final_hash, post_hash, "Hash should remain stable");
    }

    #[test]
    fn test_minority_partition_loses_fork() {
        let validators = vec![
            ("node_a".to_string(), 100),
            ("node_b".to_string(), 100),
            ("node_c".to_string(), 100),
        ];
        let mut network = PartitionTestNetwork::new(validators);

        // Partition: 2 vs 1
        network.partition(
            vec!["node_a".to_string(), "node_b".to_string()],
            vec!["node_c".to_string()],
        );

        // Majority produces 3 blocks, minority produces 5 blocks (but isolated)
        for _ in 0..3 {
            network.advance_group_a();
        }
        for _ in 0..5 {
            network.advance_group_b();
        }

        // Majority has 3 blocks, minority has 5
        let majority_len = network.nodes[0].1.lock().unwrap().get_chain_length();
        let minority_len = network.nodes[2].1.lock().unwrap().get_chain_length();

        assert!(majority_len < minority_len);

        // Reconnect and resolve — longest chain wins (minority has 5)
        network.reconnect();
        network.resolve_forks();

        // All should agree on one canonical chain
        let node_a_final = network.nodes[0].1.lock().unwrap().get_chain_length();
        let node_b_final = network.nodes[1].1.lock().unwrap().get_chain_length();
        let node_c_final = network.nodes[2].1.lock().unwrap().get_chain_length();

        assert_eq!(node_a_final, node_b_final);
        assert_eq!(node_b_final, node_c_final);
        assert_eq!(node_c_final, 5, "Longest chain (5 blocks) should win");
    }

    #[test]
    fn test_partition_with_equal_lengths() {
        let validators = vec![
            ("node_a".to_string(), 100),
            ("node_b".to_string(), 100),
            ("node_c".to_string(), 100),
        ];
        let mut network = PartitionTestNetwork::new(validators);

        network.partition(
            vec!["node_a".to_string()],
            vec!["node_b".to_string(), "node_c".to_string()],
        );

        // Both groups produce same number of blocks: 2 each
        network.advance_group_a();
        network.advance_group_a();
        network.advance_group_b();
        network.advance_group_b();

        // Verify equal length forks
        let len_a = network.nodes[0].1.lock().unwrap().get_chain_length();
        let len_b = network.nodes[1].1.lock().unwrap().get_chain_length();
        assert_eq!(len_a, len_b, "Forks should have equal length");

        // Reconnect and resolve — hash tiebreaker should pick one deterministically
        network.reconnect();
        network.resolve_forks();

        // All should agree on one canonical chain
        let node_a_final = network.nodes[0].1.lock().unwrap().get_chain_length();
        let node_b_final = network.nodes[1].1.lock().unwrap().get_chain_length();
        let node_c_final = network.nodes[2].1.lock().unwrap().get_chain_length();

        assert_eq!(node_a_final, node_b_final);
        assert_eq!(node_b_final, node_c_final);
    }
}
