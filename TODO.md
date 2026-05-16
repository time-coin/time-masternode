# TIME Coin — Outstanding Work

## Network Consolidation Refactor (5-phase plan)

### Status
- [x] Phase 1 (`3b5609c`) — MempoolSync fixes, tx_finalized rate limit raised
- [x] Phase 2 (`37fa014`) — MessageLoopConfig rate_limiter, ConnectionDriver
- [x] Phase 3 (`2332ed3`) — server.rs 3354→917 lines, drive_inbound created
- [x] Phase 4 (`cae7375`) — removed 28 redundant arms (−504 lines) from drive_inbound
- [ ] **Phase 5 — IN PROGRESS** — route remaining inline handlers in `drive_inbound` through `MessageHandler`
- [ ] Phase 6 (future) — replace `drive_inbound`'s manual loop with `run_message_loop_unified`

### Phase 5 Details

**Goal**: Eliminate the ~9 remaining inline message arms in `drive_inbound`
(`src/network/connection_driver.rs`) by routing them through the shared
`MessageHandler::handle_message` path that the outbound path already uses.

**Root cause of duplication**: Inbound handlers originated in `server.rs`; outbound
handlers lived in `peer_connection.rs` / `message_handler.rs`. Phase 3 moved inbound
into `drive_inbound` but kept its own inline arms because `MessageContext` was
outbound-only at the time.

**Changes needed**:
1. `src/network/message_handler.rs`
   - Add `seen_tx_finalized: Option<Arc<DeduplicationFilter>>` to `MessageContext`
   - Add `seen_utxo_locks: Option<Arc<DeduplicationFilter>>` to `MessageContext`
   - Add `handle_transaction_finalized()` method to `MessageHandler`
   - Wire `seen_utxo_locks` dedup into `handle_utxo_state_update()`

2. `src/network/connection_driver.rs`
   - In `drive_inbound`'s `_` fallback: populate `seen_blocks`, `seen_transactions`,
     `seen_tx_finalized`, `seen_utxo_locks` on the `MessageContext` before delegating
   - Remove inline arms for: `TransactionBroadcast`, `BlockResponse`, `BlockAnnouncement`,
     `TimeVoteRequest`, `TimeVoteResponse`, `TimeProofBroadcast`, `FinalityVoteBroadcast`,
     `TransactionVoteRequest`, `TransactionVoteResponse`, `TransactionFinalized`
   - **Keep** inline arms for: `Ping`/`Pong` (streak state), `UTXOStateUpdate`
     (per-TX flood counter `peer_tx_lock_counts`), `ChainWorkResponse`,
     `ChainWorkAtResponse`, `BlockHashResponse`, `Ack`, `Subscribe`

**Arms to keep (have per-connection mutable state that can't move to MessageContext)**:
- `Ping` / `Pong` — `ping_excess_streak`
- `UTXOStateUpdate` — `peer_tx_lock_counts: HashMap<[u8;32], u32>`
- Low-complexity plumbing arms: `ChainWork*`, `Ack`, `Subscribe`

**Validation**: `cargo fmt && cargo check && cargo clippy -- -D warnings && cargo test`
Expected: 8 pre-existing failures unrelated to this work (consensus + TLS tests).

### Phase 6 Details (future)

Replace `drive_inbound`'s manual message loop with `run_message_loop_unified` from
`peer_connection.rs`. Currently deferred because inbound vs outbound differ in
handshake direction and handshake state management. Requires careful alignment of
`MessageLoopConfig` for the inbound case.

---

## Testing

The refactor phases above deliberately preserve existing behavior — no new test
coverage was added alongside the structural changes. Once Phase 5 and Phase 6 are
complete, the following tests should be added:

### Unit tests (`src/network/message_handler.rs` or a `#[cfg(test)]` module)
- `handle_transaction_finalized` — happy path (known TX, unknown TX, ghost special_data guard)
- `handle_transaction_finalized` — dedup via `seen_tx_finalized` (second call is no-op)
- `handle_utxo_state_update` — dedup via `seen_utxo_locks` (second call is no-op)
- `MessageContext::from_registry` — verify `seen_tx_finalized` / `seen_utxo_locks` fields are populated when provided

### Integration tests (`tests/`)
- Both inbound (`drive_inbound`) and outbound (`run_message_loop_unified`) paths
  produce identical results when receiving the same message types:
  `TransactionBroadcast`, `BlockAnnouncement`, `TimeVoteRequest`, `TransactionFinalized`
- After Phase 6: single integration test that exercises a full inbound connection
  lifecycle through `run_message_loop_unified` (replacing the inbound-specific test)

### Existing test files to extend
- `tests/multi_node_consensus.rs` — add an inbound-originated transaction that reaches finality
- `tests/finalized_transaction_protection.rs` — verify `TransactionFinalized` dedup
  prevents double-finalization when received from both inbound and outbound peers
