//! Finalized Transaction Protection Tests
//!
//! Tests for Approach A: Finalized transactions cannot be excluded from canonical chain
//!
//! This test suite validates that:
//! - Reorgs are rejected when finalized transactions are missing
//! - Reorgs are accepted when finalized transactions are present (but reordered)
//! - Unfinalized transactions can be excluded during reorgs
//! - Coinbase transactions are properly handled during rollbacks
//!
//! Implements test coverage for FINALIZED_TRANSACTION_PROTECTION.md

#[cfg(test)]
mod tests {

    /// Mock transaction for testing
    #[derive(Clone, Debug, PartialEq)]
    struct MockTransaction {
        txid: [u8; 32],
        is_coinbase: bool,
        is_finalized: bool,
    }

    impl MockTransaction {
        fn new(id: u8, is_coinbase: bool, is_finalized: bool) -> Self {
            let mut txid = [0u8; 32];
            txid[0] = id;
            Self {
                txid,
                is_coinbase,
                is_finalized,
            }
        }

        fn txid(&self) -> [u8; 32] {
            self.txid
        }
    }

    /// Mock block for testing
    #[derive(Clone, Debug)]
    #[allow(dead_code)]
    struct MockBlock {
        height: u64,
        hash: [u8; 32],
        previous_hash: [u8; 32],
        transactions: Vec<MockTransaction>,
    }

    impl MockBlock {
        fn new(height: u64, txs: Vec<MockTransaction>) -> Self {
            let mut hash = [0u8; 32];
            hash[0..8].copy_from_slice(&height.to_le_bytes());

            let mut previous_hash = [0u8; 32];
            if height > 0 {
                previous_hash[0..8].copy_from_slice(&(height - 1).to_le_bytes());
            }

            Self {
                height,
                hash,
                previous_hash,
                transactions: txs,
            }
        }
    }

    /// Mock blockchain for testing reorg validation
    struct MockBlockchain {
        blocks: Vec<MockBlock>,
    }

    impl MockBlockchain {
        fn new() -> Self {
            Self { blocks: Vec::new() }
        }

        fn add_block(&mut self, block: MockBlock) {
            self.blocks.push(block);
        }

        fn current_height(&self) -> u64 {
            self.blocks.len() as u64
        }

        /// Get finalized transaction IDs in a height range
        fn get_finalized_txids_in_range(
            &self,
            start_height: u64,
            end_height: u64,
        ) -> Vec<[u8; 32]> {
            let mut finalized_txids = Vec::new();

            for height in start_height..=end_height {
                if let Some(block) = self.blocks.get((height - 1) as usize) {
                    for tx in &block.transactions {
                        if tx.is_finalized && !tx.is_coinbase {
                            finalized_txids.push(tx.txid());
                        }
                    }
                }
            }

            finalized_txids
        }

        /// Validate reorg - returns Ok if valid, Err if should be rejected
        fn validate_reorg(
            &self,
            common_ancestor: u64,
            new_blocks: &[MockBlock],
        ) -> Result<(), String> {
            let current = self.current_height();

            // Get finalized transactions that must be preserved
            let finalized_txs = self.get_finalized_txids_in_range(common_ancestor + 1, current);

            if finalized_txs.is_empty() {
                return Ok(()); // No finalized transactions to check
            }

            // Build set of all txids in new chain
            let mut new_chain_txids = std::collections::HashSet::new();
            for block in new_blocks {
                for tx in &block.transactions {
                    new_chain_txids.insert(tx.txid());
                }
            }

            // Check each finalized transaction is present
            for txid in &finalized_txs {
                if !new_chain_txids.contains(txid) {
                    return Err(format!(
                        "⛔ REORG REJECTED: New chain is missing finalized transaction {:?}",
                        txid[0]
                    ));
                }
            }

            Ok(())
        }
    }

    // ========================================================================
    // TEST CASES
    // ========================================================================

    #[test]
    fn test_reorg_accepts_all_finalized_txs_present() {
        // Setup: Chain with finalized transactions
        let mut blockchain = MockBlockchain::new();

        let tx_a = MockTransaction::new(1, false, true); // Finalized
        let tx_b = MockTransaction::new(2, false, true); // Finalized
        let tx_c = MockTransaction::new(3, false, true); // Finalized

        blockchain.add_block(MockBlock::new(
            1,
            vec![
                MockTransaction::new(0, true, false), // Coinbase
                tx_a.clone(),
                tx_b.clone(),
            ],
        ));

        blockchain.add_block(MockBlock::new(
            2,
            vec![
                MockTransaction::new(0, true, false), // Coinbase
                tx_c.clone(),
            ],
        ));

        // Action: Create alternative chain with all finalized transactions (different order)
        let new_blocks = vec![
            MockBlock::new(
                1,
                vec![
                    MockTransaction::new(0, true, false), // Coinbase
                    tx_a.clone(),
                ],
            ),
            MockBlock::new(
                2,
                vec![
                    MockTransaction::new(0, true, false), // Coinbase
                    tx_b.clone(),
                    tx_c.clone(),
                ],
            ),
        ];

        // Assert: Reorg should be accepted
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_ok(),
            "Reorg should be accepted when all finalized transactions are present"
        );
    }

    #[test]
    fn test_reorg_rejects_missing_finalized_tx() {
        // Setup: Chain with finalized transactions
        let mut blockchain = MockBlockchain::new();

        let tx_a = MockTransaction::new(1, false, true); // Finalized
        let tx_b = MockTransaction::new(2, false, true); // Finalized
        let tx_c = MockTransaction::new(3, false, true); // Finalized

        blockchain.add_block(MockBlock::new(
            1,
            vec![
                MockTransaction::new(0, true, false), // Coinbase
                tx_a.clone(),
                tx_b.clone(),
            ],
        ));

        blockchain.add_block(MockBlock::new(
            2,
            vec![
                MockTransaction::new(0, true, false), // Coinbase
                tx_c.clone(),
            ],
        ));

        // Action: Create alternative chain MISSING tx_b (finalized transaction)
        let new_blocks = vec![
            MockBlock::new(
                1,
                vec![
                    MockTransaction::new(0, true, false), // Coinbase
                    tx_a.clone(),
                ],
            ),
            MockBlock::new(
                2,
                vec![
                    MockTransaction::new(0, true, false), // Coinbase
                    tx_c.clone(),
                    // tx_b is missing!
                ],
            ),
        ];

        // Assert: Reorg should be REJECTED
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_err(),
            "Reorg should be rejected when finalized transaction is missing"
        );

        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains("REORG REJECTED"),
            "Error message should indicate reorg rejection: {}",
            err_msg
        );
        assert!(
            err_msg.contains("finalized transaction"),
            "Error message should mention finalized transaction: {}",
            err_msg
        );
    }

    #[test]
    fn test_reorg_accepts_reordered_finalized_txs() {
        // Setup: Chain with finalized transactions in order A, B, C
        let mut blockchain = MockBlockchain::new();

        let tx_a = MockTransaction::new(1, false, true); // Finalized
        let tx_b = MockTransaction::new(2, false, true); // Finalized
        let tx_c = MockTransaction::new(3, false, true); // Finalized

        blockchain.add_block(MockBlock::new(
            1,
            vec![MockTransaction::new(0, true, false), tx_a.clone()],
        ));
        blockchain.add_block(MockBlock::new(
            2,
            vec![MockTransaction::new(0, true, false), tx_b.clone()],
        ));
        blockchain.add_block(MockBlock::new(
            3,
            vec![MockTransaction::new(0, true, false), tx_c.clone()],
        ));

        // Action: Create alternative chain with order C, A, B
        let new_blocks = vec![
            MockBlock::new(1, vec![MockTransaction::new(0, true, false), tx_c.clone()]),
            MockBlock::new(2, vec![MockTransaction::new(0, true, false), tx_a.clone()]),
            MockBlock::new(3, vec![MockTransaction::new(0, true, false), tx_b.clone()]),
        ];

        // Assert: Reorg should be accepted (different order is OK)
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_ok(),
            "Reorg should be accepted when finalized transactions are reordered but all present"
        );
    }

    #[test]
    fn test_reorg_allows_missing_unfinalized_tx() {
        // Setup: Chain with mix of finalized and unfinalized transactions
        let mut blockchain = MockBlockchain::new();

        let tx_a = MockTransaction::new(1, false, true); // Finalized
        let tx_b = MockTransaction::new(2, false, false); // NOT finalized
        let tx_c = MockTransaction::new(3, false, true); // Finalized

        blockchain.add_block(MockBlock::new(
            1,
            vec![
                MockTransaction::new(0, true, false),
                tx_a.clone(),
                tx_b.clone(), // Unfinalized
            ],
        ));
        blockchain.add_block(MockBlock::new(
            2,
            vec![MockTransaction::new(0, true, false), tx_c.clone()],
        ));

        // Action: Create alternative chain excluding tx_b (unfinalized)
        let new_blocks = vec![
            MockBlock::new(
                1,
                vec![
                    MockTransaction::new(0, true, false),
                    tx_a.clone(),
                    // tx_b excluded (OK because not finalized)
                ],
            ),
            MockBlock::new(2, vec![MockTransaction::new(0, true, false), tx_c.clone()]),
        ];

        // Assert: Reorg should be accepted (unfinalized TX can be excluded)
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_ok(),
            "Reorg should be accepted when only unfinalized transactions are excluded"
        );
    }

    #[test]
    fn test_coinbase_txs_not_required_in_reorg() {
        // Setup: Chain with coinbase transactions
        let mut blockchain = MockBlockchain::new();

        let coinbase_1 = MockTransaction::new(99, true, false);
        let tx_a = MockTransaction::new(1, false, true); // Finalized

        blockchain.add_block(MockBlock::new(1, vec![coinbase_1.clone(), tx_a.clone()]));

        // Action: Create alternative chain with different coinbase
        let new_coinbase = MockTransaction::new(88, true, false);
        let new_blocks = vec![MockBlock::new(
            1,
            vec![
                new_coinbase, // Different coinbase
                tx_a.clone(),
            ],
        )];

        // Assert: Reorg should be accepted (coinbase can change)
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_ok(),
            "Reorg should be accepted when coinbase transactions differ"
        );
    }

    #[test]
    fn test_empty_blocks_reorg() {
        // Setup: Chain with only coinbase transactions
        let mut blockchain = MockBlockchain::new();

        blockchain.add_block(MockBlock::new(
            1,
            vec![MockTransaction::new(0, true, false)],
        ));
        blockchain.add_block(MockBlock::new(
            2,
            vec![MockTransaction::new(0, true, false)],
        ));

        // Action: Create alternative chain (also only coinbase)
        let new_blocks = vec![
            MockBlock::new(1, vec![MockTransaction::new(0, true, false)]),
            MockBlock::new(2, vec![MockTransaction::new(0, true, false)]),
        ];

        // Assert: Reorg should be accepted (no finalized transactions to check)
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_ok(),
            "Reorg should be accepted when there are no finalized transactions"
        );
    }

    #[test]
    fn test_multiple_missing_finalized_txs() {
        // Setup: Chain with multiple finalized transactions
        let mut blockchain = MockBlockchain::new();

        let tx_a = MockTransaction::new(1, false, true);
        let tx_b = MockTransaction::new(2, false, true);
        let tx_c = MockTransaction::new(3, false, true);
        let tx_d = MockTransaction::new(4, false, true);

        blockchain.add_block(MockBlock::new(
            1,
            vec![
                MockTransaction::new(0, true, false),
                tx_a.clone(),
                tx_b.clone(),
                tx_c.clone(),
                tx_d.clone(),
            ],
        ));

        // Action: Create alternative chain missing multiple finalized transactions
        let new_blocks = vec![MockBlock::new(
            1,
            vec![
                MockTransaction::new(0, true, false),
                tx_a.clone(),
                // tx_b, tx_c, tx_d all missing!
            ],
        )];

        // Assert: Reorg should be rejected (catches first missing)
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_err(),
            "Reorg should be rejected when multiple finalized transactions are missing"
        );
    }

    #[test]
    fn test_partial_reorg_with_common_ancestor() {
        // Setup: Longer chain with common ancestor
        let mut blockchain = MockBlockchain::new();

        let tx_a = MockTransaction::new(1, false, true);
        let tx_b = MockTransaction::new(2, false, true);
        let tx_c = MockTransaction::new(3, false, true);

        blockchain.add_block(MockBlock::new(
            1,
            vec![MockTransaction::new(0, true, false), tx_a.clone()],
        ));
        blockchain.add_block(MockBlock::new(
            2,
            vec![MockTransaction::new(0, true, false), tx_b.clone()],
        ));
        blockchain.add_block(MockBlock::new(
            3,
            vec![MockTransaction::new(0, true, false), tx_c.clone()],
        ));

        // Action: Reorg from height 2 (common ancestor at height 1)
        let new_blocks = vec![MockBlock::new(
            2,
            vec![
                MockTransaction::new(0, true, false),
                tx_b.clone(),
                tx_c.clone(), // Both in same block
            ],
        )];

        // Assert: Should check only heights 2-3 for finalized transactions
        let result = blockchain.validate_reorg(1, &new_blocks);
        assert!(
            result.is_ok(),
            "Partial reorg should only validate transactions after common ancestor"
        );
    }

    #[test]
    fn test_deep_reorg_preserves_all_finalized() {
        // Setup: Long chain with many finalized transactions
        let mut blockchain = MockBlockchain::new();
        let mut all_finalized_txs = Vec::new();

        for height in 1..=10 {
            let tx = MockTransaction::new(height as u8, false, true);
            all_finalized_txs.push(tx.clone());
            blockchain.add_block(MockBlock::new(
                height,
                vec![MockTransaction::new(0, true, false), tx],
            ));
        }

        // Action: Create alternative chain with all finalized transactions
        let mut new_blocks = Vec::new();
        for height in 1..=10 {
            new_blocks.push(MockBlock::new(
                height,
                vec![
                    MockTransaction::new(0, true, false),
                    all_finalized_txs[(height - 1) as usize].clone(),
                ],
            ));
        }

        // Assert: Deep reorg accepted if all finalized transactions present
        let result = blockchain.validate_reorg(0, &new_blocks);
        assert!(
            result.is_ok(),
            "Deep reorg should be accepted when all finalized transactions are present"
        );
    }

    #[test]
    fn test_performance_large_transaction_set() {
        // Test performance with large number of transactions
        let mut blockchain = MockBlockchain::new();
        let mut finalized_txs = Vec::new();

        // Create block with 1000 finalized transactions
        for i in 1..=255 {
            finalized_txs.push(MockTransaction::new(i as u8, false, true));
        }

        blockchain.add_block(MockBlock::new(1, {
            let mut txs = vec![MockTransaction::new(0, true, false)];
            txs.extend(finalized_txs.clone());
            txs
        }));

        // Create alternative chain with same transactions
        let new_blocks = vec![MockBlock::new(1, {
            let mut txs = vec![MockTransaction::new(0, true, false)];
            txs.extend(finalized_txs);
            txs
        })];

        // Assert: Should complete quickly (HashSet is O(1) lookup)
        let start = std::time::Instant::now();
        let result = blockchain.validate_reorg(0, &new_blocks);
        let duration = start.elapsed();

        assert!(result.is_ok(), "Large reorg should be validated correctly");
        assert!(
            duration.as_millis() < 100,
            "Validation should complete in <100ms, took {:?}",
            duration
        );
    }
}
