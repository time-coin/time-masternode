# 🔧 time-cli - Bitcoin-like RPC Client

## ✨ Overview

`time-cli` is a command-line tool for interacting with the TIME Coin daemon (`timed`) using Bitcoin-compatible RPC commands.

---

## 🚀 Quick Start

```bash
# Build
cargo build --release

# Basic usage (pretty JSON output by default)
time-cli getblockchaininfo

# Compact JSON output (single line)
time-cli --compact getblockchaininfo

# Human-readable output
time-cli --human getblockchaininfo

# With custom RPC URL
time-cli --rpc-url http://192.168.1.100:24101 getnetworkinfo
```

---

## 📊 Output Formats

TIME CLI supports three output formats:

### 1. Pretty JSON (Default)
```bash
time-cli getblockchaininfo
```
Returns formatted JSON (like Bitcoin Core):
```json
{
  "chain": "main",
  "blocks": 1,
  "consensus": "TimeVote",
  "instant_finality": true
}
```

### 2. Compact JSON
```bash
time-cli --compact getblockchaininfo
```
Returns single-line JSON for scripting:
```json
{"chain":"main","blocks":1,"consensus":"TimeVote","instant_finality":true}
```

### 3. Human-Readable
```bash
time-cli --human getblockchaininfo
```
Returns formatted text output:
```
Blockchain Information:
  Chain:            main
  Blocks:           1
  Consensus:        TimeVote
  Instant Finality: true
```

**Supported for --human flag:**
- `getblockchaininfo` - Formatted info
- `getblockcount` - Simple height display
- `getbalance` - Balance with TIME label
- `listunspent` - Table format
- `masternodelist` / `masternode list` - Table format
- `masternodestatus` / `masternode status` - Formatted status
- `getpeerinfo` - Table format
- `uptime` - Days/hours for >=1 hour; minutes for sub-hour nodes
- All other commands default to pretty JSON

---

## 📋 Available Commands

### Blockchain Information

#### Get Blockchain Info
```bash
time-cli getblockchaininfo
```
Returns general blockchain information including chain, blocks, consensus type, and finality.

#### Get Block Count
```bash
time-cli getblockcount
```
Returns the current block height.

#### Get Best Block Hash
```bash
time-cli getbestblockhash
```
Returns the hash of the current chain tip block.

#### Get Block Hash
```bash
time-cli getblockhash <height>
```
Returns the block hash at a given height.

#### Get Block Header
```bash
time-cli getblockheader <height_or_hash>
```
Returns block header data (timestamp, previous hash, merkle root, etc.) without full transaction detail.

#### Get Block
```bash
time-cli getblock 1
```
Returns information about a specific block by height.

#### Find Block by Date
```bash
time-cli findblockbydate <unix_timestamp>
```
Binary-searches the chain for the block closest to the given Unix timestamp.
Returns the block `height`, its actual `timestamp`, and `delta_secs` — how far
it is from the requested time.

```bash
# Example: find the block produced closest to 2026-01-01 00:00 UTC
time-cli findblockbydate 1735689600
```

#### Get Supply
```bash
time-cli getsupply
time-cli getsupply 5
```
Returns total circulating supply. Optionally pass a `dormant_years` argument to
exclude coins that have not moved in that many years (useful for estimating
effectively circulating supply).

---

### Network Information

#### Get Network Info
```bash
time-cli getnetworkinfo
```
Returns network information including version, protocol, and connections.

#### Get Connection Count
```bash
time-cli getconnectioncount
```
Returns the number of active peer connections.

#### Get Peer Info
```bash
time-cli getpeerinfo
```
Returns information about connected peers.

---

### Peer Ban Management

```bash
time-cli getbanlist
time-cli ban 154.217.246.86
time-cli ban 154.217.246.86 --reason "attacking node"
time-cli unban 154.217.246.86
time-cli unbansubnet 154.217.246.0/24
time-cli clearbanlist
```

Use `ban` to permanently ban a single IP, `unban` to lift a single-IP ban,
`unbansubnet` for one CIDR subnet, and `clearbanlist` to remove all IP bans,
subnet bans, and violation counters.

#### Aggregate Ban Lists (Multi-Node)
```bash
time-cli aggregatebanlistss
time-cli aggregatebanlistss http://node1:24001 http://node2:24001
```
Queries the local node plus any provided URLs, merges all ban lists, and prints
a consolidated report. Useful for operators running multiple nodes who want to
see which IPs are banned across the fleet and propagate bans manually.

---

### Peer Whitelist Management

Whitelisted peers bypass rate limiting and are preferred for chain sync.

```bash
time-cli addwhitelist 1.2.3.4
time-cli getwhitelist
time-cli removewhitelist 1.2.3.4
```

#### Reset Peer Profiles
```bash
time-cli resetpeerprofiles
```
Clears all learned peer scoring data from the AI subsystem. Use when peer
scores become stale after a network topology change.

---

### UTXO & Transactions

#### Get UTXO Set Info
```bash
time-cli gettxoutsetinfo
```
Returns statistics about the UTXO set (total UTXOs, total amount, etc.).

#### Get TX Output
```bash
time-cli gettxout <txid> <vout>
```
Returns details about a specific unspent transaction output, or null if it is
spent.

#### Get Transaction
```bash
time-cli gettransaction <txid>
```
Returns information about a specific transaction.

#### Get Raw Transaction
```bash
time-cli getrawtransaction <txid>
time-cli getrawtransaction <txid> --verbose
```
Returns raw transaction data. With `--verbose`, returns a decoded JSON object.

#### Create Raw Transaction
```bash
time-cli createrawtransaction '[{"txid":"<txid>","vout":0}]' '{"<address>":10.0}'
```
Constructs an unsigned raw transaction from the provided inputs and outputs.
The result is a hex string that can be signed and broadcast with
`sendrawtransaction`.

#### Decode Raw Transaction
```bash
time-cli decoderawtransaction <hex>
```
Decodes a raw transaction hex string and returns a JSON object with all fields.

#### Send Raw Transaction
```bash
time-cli sendrawtransaction <hex>
```
Broadcasts a raw transaction to the network.

#### Rebroadcast Transaction
```bash
time-cli rebroadcasttransaction <txid>
```
Re-announces a transaction to all connected peers. Use when a transaction was
submitted but may not have fully propagated (e.g. due to a brief network
partition).

#### Get Transaction Finality
```bash
time-cli gettransactionfinality <txid>
```
Returns the current finality status of a transaction: vote counts, whether the
67% threshold has been reached, and whether a TimeProof has been assembled.

#### Estimate Smart Fee
```bash
time-cli estimatesmartfee <conf_target>
```
Returns the estimated fee rate (TIME/kB) needed to confirm within `conf_target`
blocks. TIME uses a tiered fee schedule; this command returns the applicable
tier rate for the given block target.

#### List Unspent
```bash
time-cli listunspent
time-cli listunspent 6 9999
```
Lists unspent transaction outputs with optional `minconf`/`maxconf` filters.

#### List Unspent (Multi-Address)
```bash
time-cli listunspentmulti '["TIME1abc...","TIME1xyz..."]'
```
Lists UTXOs for multiple addresses in one call. Pass a JSON array of addresses.

#### List Transactions
```bash
time-cli listtransactions
time-cli listtransactions 20
```
Lists recent wallet transactions (default 10, max specified by count argument). Each entry includes `txid`, `category` (send/receive/consolidation), `amount`, `confirmations`, and `time`. If the transaction contains an encrypted memo that this wallet can decrypt, a `"memo"` field is included in the output.

Block reward distributions include an encrypted `"Block Reward"` memo (self-send encrypted, visible only to the block-producing node).

Example output entry with memo:
```json
{
  "txid": "7ce5821a2faf...",
  "category": "consolidation",
  "amount": -2.59,
  "confirmations": 5,
  "time": 1710441600,
  "memo": "UTXO Consolidation"
}
```

#### List Received by Address
```bash
time-cli listreceivedbyaddress
time-cli listreceivedbyaddress 6 true
```
Returns total received per address with confirmation count. Pass `minconf` and
`include_empty` (true/false) to filter.

---

### Memory Pool

#### Get Mempool Info
```bash
time-cli getmempoolinfo
```
Returns memory pool statistics (count, bytes, fees).

#### Get Raw Mempool
```bash
time-cli getrawmempool
time-cli getrawmempool --verbose
```
Returns list of transactions in the memory pool. With `--verbose`, includes fee
and time details per transaction.

#### Get Mempool Verbose
```bash
time-cli getmempoolverbose
```
Returns full detail for every transaction in the mempool, including fee, size,
inputs/outputs, and finality vote counts. More detailed than `getrawmempool --verbose`.

#### Drop Transaction
```bash
time-cli droptransaction <txid>
```
Removes a specific transaction from the local mempool. Does not broadcast the
removal to peers; use when a transaction is stuck locally and you want to
resubmit it.

#### Clear Stuck Transactions
```bash
time-cli clearstucktransactions
```
Removes transactions that have been in the mempool longer than the eviction
threshold. Use when the mempool contains abandoned transactions that are
blocking UTXO cleanup.

---

### Masternode Operations

#### Generate Masternode Key
```bash
time-cli masternode genkey
```
Generates a new masternode private key (base58check-encoded Ed25519). Add the output to `masternodeprivkey=` in `time.conf`.

#### List Masternodes
```bash
time-cli masternode list
```
Returns list of all masternodes with their status, tier, and collateral lock status.

**Output includes:**
- Address
- Tier (Free, Bronze, Silver, Gold)
- Active status
- Uptime
- Collateral status (Locked or Legacy)

#### Masternode Status
```bash
time-cli masternode status
```
Returns status of this node's masternode (if configured).

> **Note:** Masternode registration and deregistration are managed via `time.conf` and `masternode.conf`. See the [Masternode Guide](MASTERNODE_GUIDE.md) for details.

> **Backward compatibility:** `masternodelist` and `masternodestatus` are still accepted as aliases.

#### Masternode Registration Status
```bash
time-cli masternoderegstatus
time-cli masternoderegstatus TIME1abc...
```
Returns the current on-chain registration status for this node (or a specific
address). Shows whether a valid registration transaction exists and when it was
last seen.

#### Register Masternode (Advanced)
```bash
time-cli masternodereg \
  --collateral <txid>:<vout> \
  --masternode-ip <ip> \
  --payout-address <TIME_address> \
  [--port <port>] \
  [--wallet-path <path>] \
  [--wallet-password <pass>] \
  [--privkey <hex>]
```
Signs and broadcasts a `MasternodeRegistration` special transaction. This is an
advanced command for operators who need manual registration control. Most
operators should use the config-based auto-registration (`masternode=1` in
`time.conf`) instead.

- `--collateral`: The outpoint (`txid:vout`) of the collateral UTXO
- `--masternode-ip`: Public IP your node is reachable on
- `--port`: P2P port (default: 24000 mainnet / 24100 testnet)
- `--payout-address`: Address that receives block rewards
- `--wallet-path` / `--wallet-password`: Load key from a specific wallet file
- `--privkey`: Supply the 32-byte Ed25519 signing key as hex (bypasses wallet load)

#### Audit Collateral
```bash
time-cli auditcollateral
```
Scans every registered masternode and reports any whose collateral UTXO is
missing, spent, or below the minimum for their declared tier. Use to identify
nodes that may be gaming the tier system.

#### Check Collateral
```bash
time-cli checkcollateral
```
Verifies that this node's configured collateral is properly locked on-chain.
Reports whether the lock exists, the outpoint, and the tier it qualifies for.

#### Find Collateral
```bash
time-cli findcollateral <txid>:<vout>
```
Looks up which masternode (if any) has registered the given outpoint as
collateral and returns its registration details.

#### List Locked Collaterals
```bash
time-cli listlockedcollaterals
```
Lists all currently locked collaterals with masternode details.

#### Release a Single Collateral Lock
```bash
time-cli releasecollateral <txid> <vout>
```
Releases the collateral lock for a specific UTXO and clears its persistent sled
anchor in one step. Use this when an old collateral is stuck after a tier upgrade
(e.g. Silver to Gold) without disturbing other active collateral locks.

```bash
# Example: free stuck Silver after upgrading to Gold
time-cli releasecollateral 38a43f69bd3f38f9f74981a8ba5ba120fe5aa7e9919b2396b7d383557757ea97 0
```

Remote nodes self-correct within 30 seconds when they receive the next gossip
announcement from the upgraded node.

#### Clear a Stale Collateral Anchor
```bash
time-cli clearcollateralanchor <txid>:<vout>
```
Deletes the `collateral_anchor` sled entry for an outpoint and auto-unbans any
IP banned for a hijack attempt on that outpoint. Use when the persistent anchor
points to the wrong node (e.g. reversed by gossip delivery order) so the
legitimate owner's next announcement can re-anchor cleanly.

```bash
time-cli clearcollateralanchor ce8b5f168aca656f6e9cca2a475f2db4b6033742c6d437f22217bb6ddb557de0:0
```

Note: `releasecollateral` also clears the anchor. Use `clearcollateralanchor`
when you only need to fix the anchor without releasing the lock (e.g. correcting
reversed anchors between two legitimate nodes).

#### Release ALL Collateral Locks
```bash
time-cli releaseallcollaterals
```
Releases every collateral lock on this node without touching transaction UTXO
locks. Active masternodes re-lock their collateral within 30 seconds via their
next gossip announcement. Use as a last resort when multiple collaterals are
stuck or a squatter has locked UTXOs belonging to legitimate nodes.

#### Reward Report
```bash
time-cli getrewardreport                           # last week (~1,008 blocks)
time-cli getrewardreport <N>                       # last N blocks (max 10,080)
time-cli getrewardreport <from_height> <to_height> # specific block range
```
Scans blocks and returns reward totals broken down by address and by tier.
Useful for auditing earnings or verifying fair distribution across the network.

Output includes `by_tier` (node count, total wins, total earned, avg per node)
and `by_address` (individual earnings sorted by amount descending), plus
`blocks_scanned` and `total_emitted`.

```bash
# Audit the last 2 weeks
time-cli getrewardreport 2016

# Check a specific block range
time-cli getrewardreport 1000 2000
```

---

### UTXO Lock Management

The daemon locks UTXOs when they are reserved for a pending transaction. These
commands let operators inspect and repair lock state without restarting the node.

#### List Locked UTXOs
```bash
time-cli listlockedutxos
```
Returns all UTXOs currently held in the lock state, including the transaction
they are reserved for and how long they have been locked.

#### Unlock a Specific UTXO
```bash
time-cli unlockutxo <txid> <vout>
```
Releases the lock on a single transaction UTXO. Use when a transaction was
dropped or abandoned and the UTXO is incorrectly reported as locked.

#### Unlock a Specific Collateral
```bash
time-cli unlockcollateral <txid> <vout>
```
Releases the lock on a collateral UTXO without going through the full
`releasecollateral` flow (which also clears the sled anchor). Use when only
the in-memory lock needs to be cleared.

#### Unlock Orphaned UTXOs
```bash
time-cli unlockorphanedutxos
```
Scans for UTXOs that are locked for transactions no longer present in the
mempool (orphans) and releases them automatically. Safe to run at any time.

#### Force Unlock All
```bash
time-cli forceunlockall
```
**Danger.** Releases every UTXO lock (transaction and collateral) in one shot.
Active masternodes will re-lock their collateral via gossip within ~30 seconds,
but any in-flight transactions will need to be resubmitted. Use only when
`unlockorphanedutxos` and `releaseallcollaterals` are insufficient.

#### Cleanup Locked UTXOs
```bash
time-cli cleanuplockedutxos
```
Removes stale entries from the UTXO lock table (e.g. entries pointing to
UTXOs that no longer exist in the UTXO set). Lighter than `forceunlockall`.

---

### Consensus Information

#### Get Consensus Info
```bash
time-cli getconsensusinfo
```
Returns information about the TimeVote consensus:
- Type (TimeVote)
- Number of masternodes
- Quorum requirements
- Finality time

#### Get TimeVote Status
```bash
time-cli gettimevotestatus
```
Returns real-time status of the TimeVote protocol: active vote rounds, number
of finalized transactions since startup, current quorum threshold, and whether
a liveness stall (51% fallback) is in effect.

---

### Governance

On-chain governance allows Bronze/Silver/Gold masternodes to submit proposals and vote on protocol changes. See [GOVERNANCE.md](GOVERNANCE.md) for the full reference.

#### Get Treasury Balance
```bash
time-cli gettreasurybalance
```
Returns the current balance of the on-chain treasury address that governance
proposals can draw from.

#### Submit a Proposal

```bash
# Treasury disbursement
time-cli submitproposal treasury <recipient_address> <amount_TIME> "<description>"

# Fee schedule change
time-cli submitproposal feeschedule <new_min_fee_TIME> '[{"upper":100,"rate_bps":100},...]'
```

Returns `{"proposal_id":"<64-hex>"}`. The proposal is broadcast to all peers immediately. Requires an unlocked wallet and an active Bronze/Silver/Gold masternode.

#### Vote on a Proposal

```bash
time-cli voteproposal <proposal_id> yes
time-cli voteproposal <proposal_id> no
```

Votes are stake-weighted (Bronze=1, Silver=10, Gold=100). A proposal passes when YES weight >= 67% of total active governance weight at the end of the 1,008-block voting window (~1 week).

#### List Proposals

```bash
# All proposals
time-cli listproposals

# Filter by status: active, passed, failed, executed
time-cli listproposals active
```

#### Get Proposal Detail

```bash
time-cli getproposal <proposal_id>
```

Returns full proposal detail including current vote tally, yes/total weight, and quorum percentage.

#### Get Fee Schedule

```bash
time-cli getfeeschedule
```

Returns the live fee schedule currently in effect on the network (minimum fee and tiered rate table). Use this to verify the current rates before sending or to inspect the result of a passed fee-schedule governance proposal.

---

### Wallet Operations

#### Get Balance
```bash
time-cli getbalance
time-cli getbalance TIME1abc...
```
Returns wallet balance. Optionally pass an address to query any address's balance.

#### Get New Address
```bash
time-cli getnewaddress
```
Generates a new TIME Coin address derived from the wallet key.

> **Note:** TIME wallets are single-key (Ed25519). `getnewaddress` returns the
> same deterministic address each time unless the wallet key changes.

#### Get Wallet Info
```bash
time-cli getwalletinfo
```
Returns wallet metadata: file path, TIME address, and public key (hex).

#### Get Local Wallet
```bash
time-cli getlocalwallet
```
Returns the wallet address configured on this running node. Useful for
identifying which address belongs to this node when managing multiple nodes.

#### Get Address Info
```bash
time-cli getaddressinfo <address>
```
Returns information about a TIME address: whether it is this wallet's address,
the public key if known, and the current balance.

#### Send to Address
```bash
time-cli sendtoaddress <address> <amount>
time-cli sendtoaddress <address> <amount> --subtract-fee
time-cli sendtoaddress <address> <amount> --memo "Payment for invoice #42"
```
Send TIME to an address. Fee is tiered (1% for amounts under 100 TIME, 0.5% under 1,000 TIME, 0.25% under 10,000 TIME, 0.1% above), with a flat minimum of 0.01 TIME. Added on top by default. Use `--subtract-fee` to deduct the fee from the send amount instead.

**Minimum send amount: 1 TIME.** Amounts below 1 TIME are rejected at the protocol level (the 0.01 TIME flat fee would represent >=1% of the amount). Self-sends (UTXO consolidation) are exempt from this minimum.

**Options:**
- `--subtract-fee` — Deduct fee from the send amount (recipient gets amount minus fee)
- `--nowait` — Return TXID immediately without waiting for finality
- `--memo <text>` — Attach an encrypted memo (max 256 chars). The memo is encrypted using ECDH (X25519) + AES-256-GCM so that only the sender and recipient can read it. Other nodes see only ciphertext on-chain.

**Memo notes:** The recipient must have at least one prior on-chain transaction for their public key to be known. If the key is unavailable, the transaction sends without a memo. Memos appear in `listtransactions` output when decryptable.

#### Send From Address
```bash
time-cli sendfrom <from_address> <to_address> <amount>
time-cli sendfrom <from_address> <to_address> <amount> --subtract-fee --nowait
```
Like `sendtoaddress` but requires specifying the source address explicitly.
Useful when a node holds multiple addresses or you want to control which UTXOs
are spent.

#### Sign Message
```bash
time-cli signmessage "Hello TIME" <address>
```
Signs an arbitrary message using the wallet's Ed25519 key and returns the
base64-encoded signature. The address must belong to this wallet.

#### Verify Message
```bash
time-cli verifymessage <address> <signature_base64> "Hello TIME"
```
Verifies a message signature. Returns `true` if the signature is valid for the
given address and message, `false` otherwise.

#### Validate Address
```bash
time-cli validateaddress <address>
```
Validates a TIME Coin address format and returns whether it is valid.

#### Dump Private Key
```bash
time-cli dumpprivkey
time-cli dumpprivkey --wallet-path /path/to/wallet.dat --wallet-password mypass
```
Exports the wallet's Ed25519 private key, public key, and address. Runs
**offline** — no running daemon required. Output includes:
```
address:    TIME1abc...
pubkey:     a1b2c3...
privkey:    d4e5f6...
```

> **Security:** Store the private key output securely. Anyone with the private
> key can spend all funds associated with the address.

#### Merge UTXOs
```bash
time-cli mergeutxos
time-cli mergeutxos --min-count 5 --max-count 50
```
Merge multiple UTXOs into one to reduce UTXO set size.

#### Request Payment
```bash
time-cli request-payment 50.0
time-cli request-payment 50.0 --memo "Invoice #42" --label "Alice's Shop"
```
Generate a payment request URI that you can share via email, text, or QR code. The URI includes your address, public key (for encrypted memos), requested amount, and an optional description.

Example output:
```
timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3...&memo=Invoice%20%2342&label=Alice%27s%20Shop
```

The payer's wallet will automatically cache your public key, enabling encrypted memo support for this and future transactions.

#### Pay a Payment Request
```bash
time-cli pay-request "timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3...&memo=Invoice%20%2342"
time-cli pay-request "timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3..." --memo "Custom note"
```
Parse a payment request URI and send the specified amount with an encrypted memo. If the URI contains a memo, it is used automatically. Use `--memo` to override with your own message.

#### Payment Request Lifecycle (Advanced)

For peer-to-peer payment request flows between nodes:

```bash
# Get pending payment requests for an address
time-cli getpaymentrequests TIME1abc...

# Respond to a payment request (accept)
time-cli respondpaymentrequest <request_id> <requester_address> <payer_address> true <txid>

# Respond to a payment request (decline)
time-cli respondpaymentrequest <request_id> <requester_address> <payer_address> false

# Cancel an outgoing request you created
time-cli cancelpaymentrequest <request_id> <requester_address>

# Mark a request as viewed
time-cli markpaymentrequestviewed <request_id> <requester_address> <payer_address>
```

#### Wallet Notes

- All amounts are in TIME (the base unit)
- Transactions achieve instant finality via TimeVote consensus
- Minimum transaction fee: 0.01 TIME (flat floor; tiered % applies for larger amounts)
- Minimum send amount: 1 TIME (non-self-sends only)
- UTXOs are locked during transaction processing; rejected transactions unlock UTXOs automatically
- Testnet addresses start with `TIME0`; mainnet addresses start with `TIME1`

---

### Secure Messaging (TIME-MSG v1)

TIME-MSG v1 is an end-to-end encrypted store-and-forward messaging layer built into `timed`. Messages are encrypted on the sender side using X25519 ECDH + XChaCha20-Poly1305 and relayed through Silver/Gold masternodes. They are never stored or transmitted in plaintext.

All nodes (Free, Bronze, Silver, Gold) can send and receive messages. Silver/Gold masternodes also act as relay nodes that store and forward envelopes.

#### Send a Secure Message
```bash
time-cli sendmessage <to_address> "<body>"
time-cli sendmessage <to_address> "<body>" --subject "Meeting tomorrow"
time-cli sendmessage <to_address> "<body>" --ttl 168
```
Encrypts `body` for the recipient and submits the envelope to Silver/Gold relay nodes. Returns a `msg_id` (64 hex chars) and relay confirmation status.

**Options:**
- `--subject <text>` — Optional subject line (max 255 bytes)
- `--ttl <hours>` — Time-to-live in hours (default 720 = 30 days, max 720)

Requires the recipient's Ed25519 pubkey, resolved via: local contacts book → UTXO pubkey cache → P2P query (5 s timeout). Fails with `PubkeyNotFound` if none of the three sources return a key.

```json
{
  "msg_id": "a3f8c1...",
  "status": "pending",
  "relay_acks": 2,
  "relay_targets": 3
}
```

#### Fetch Incoming Messages
```bash
time-cli getmessages
time-cli getmessages --since 1748736000
time-cli getmessages --limit 20
```
Fetches and decrypts all envelopes addressed to this wallet from the local relay store and connected peers. Automatically sends read receipts when requested by the sender and caches each sender's pubkey in your contacts book.

**Options:**
- `--since <unix_timestamp>` — Only fetch messages received after this time (default: last 30 days)
- `--limit <n>` — Maximum number of messages to return

Each message in the result includes `msg_id`, `from`, `subject`, `body`, `timestamp`, `ttl_seconds`, and `read_receipt_requested`.

#### Check Message Status
```bash
time-cli getmessagestatus <msg_id>
```
Returns delivery/read-receipt status for a message you sent. Queries the local relay store and broadcasts a `MsgAckQuery` to peers before responding.

Status values: `pending` | `delivered` | `read` | `expired` | `failed`

#### Look Up a Pubkey
```bash
time-cli getpubkey <address>
```
Resolves the Ed25519 public key for a TIME address using the same 3-source chain as `sendmessage`. Useful for verifying you have the correct key before sending.

#### Contacts Book

The local contacts book caches pubkeys by TIME address for instant future sends without a P2P lookup.

```bash
# Add or update a contact
time-cli addcontact <address>
time-cli addcontact <address> --label "Alice"

# List all saved contacts
time-cli listcontacts

# Remove a contact
time-cli removecontact <address>
```

Contacts are automatically added when you receive a message from an address.

---

### Secure Messaging — RPC-Only Methods (Wallet Bridge)

These four methods are for wallets that hold their own keys and handle encryption locally (forwarded-address masternodes). They have no corresponding `time-cli` subcommand.

| Method | Description |
|--------|-------------|
| `getrawenvelopes` | Fetch raw CBOR-serialized envelopes for a TIME address without decrypting |
| `submitenvelope` | Submit a pre-encrypted CBOR envelope; stored locally and forwarded to Silver/Gold relay peers |
| `lookuppubkey` | Resolve Ed25519 pubkey for any TIME address (same 3-source chain as `sendmessage`) |
| `publishpubkey` | Broadcast this node's Ed25519 pubkey via `MsgPubkeyResponse` to all connected peers |

---

### Daemon Control

#### Get Info
```bash
time-cli getinfo
```
Returns basic node info: version, block height, connections, balance.
Equivalent to the deprecated `getinfo` in Bitcoin Core; TIME retains it for
compatibility with monitoring scripts.

#### Get Uptime
```bash
time-cli uptime
```
Returns daemon uptime in seconds.

#### Stop Daemon
```bash
time-cli stop
```
Stops the daemon gracefully.

---

### Chain Maintenance & Recovery

These commands are for operators who need to repair state or recover from a fork. All run against the **live daemon** via RPC.

#### Get Transaction Index Status
```bash
time-cli gettxindexstatus
```
Reports whether the transaction index is fully built, currently being built, or
missing. Use to verify the index is ready before relying on `gettransaction` for
historical lookups.

#### Reindex (UTXO + Transaction Index)
```bash
time-cli reindex
```
Rebuilds the UTXO set and transaction index by replaying all blocks from genesis. Use this to fix stale balances after chain corruption. Runs synchronously — the CLI waits for completion.

```bash
time-cli reindextransactions
```
Rebuilds only the transaction index in the background (returns immediately).

#### Deep Fork Recovery
If a node is stuck on a minority fork more than 100 blocks deep, normal reorg logic is blocked by the finality guard. Use the two-step recovery sequence:

```bash
# Step 1 — clear the BFT finality lock
time-cli resetfinalitylock 0

# Step 2 — roll back to genesis and resync from whitelisted peers
time-cli resyncfromwhitelist 0
```

`resyncfromwhitelist` bypasses the MAX_REORG_DEPTH (100-block) limit and re-downloads the canonical chain from trusted peers. Requires at least one whitelisted peer to be connected.

The `update.sh` script wraps this as a single command:
```bash
sudo ./scripts/update.sh resync           # both networks
sudo ./scripts/update.sh resync mainnet   # mainnet only
```

#### Full Chain Reset
```bash
time-cli rollbacktoblock0
```
**Danger.** Deletes all blocks above genesis, clears UTXOs, and resets chain height to 0. The node will re-download the entire chain from peers on restart. Use only when `resyncfromwhitelist` fails (e.g. no whitelisted peers reachable).

#### Rollback to Height
```bash
time-cli rollbacktoheight <height>
```
**Danger.** Rolls back the chain to a specific height (max 100 blocks, enforced by the finality guard). Use `resyncfromwhitelist` for deeper rollbacks.

---

## 🔧 Configuration

### Default RPC URL
```
Mainnet: http://127.0.0.1:24001 (default)
Testnet: http://127.0.0.1:24101 (use --testnet flag)
```

### Network Selection
```bash
# Mainnet (default)
time-cli getblockcount

# Testnet
time-cli --testnet getblockcount
```

### Custom RPC URL
```bash
time-cli --rpc-url http://node.example.com:24001 getblockcount
```

### RPC Authentication
```bash
# Supply credentials via flags (overrides time.conf and .cookie)
time-cli --rpcuser alice --rpcpassword secret getblockchaininfo

# Credentials are automatically read from time.conf or .cookie file when present
```

### Output Format Options

```bash
# Pretty JSON (default) - matches Bitcoin Core
time-cli getbalance

# Compact JSON - single line for scripts
time-cli --compact getbalance

# Human-readable - formatted text output
time-cli --human getbalance
```

---

## 📊 Output Format

All commands return JSON output by default (matching Bitcoin Core):

```json
{
  "chain": "main",
  "blocks": 1,
  "consensus": "TimeVote",
  "instant_finality": true
}
```

**Format Options:**
- Default: Pretty JSON (formatted, multiple lines)
- `--compact`: Single-line JSON for scripting
- `--human`: Human-readable formatted text

---

## 💡 Usage Examples

### Check if node is running
```bash
time-cli uptime
```

### Get consensus status
```bash
time-cli getconsensusinfo
```

### List all masternodes
```bash
time-cli masternode list
```

### Get blockchain info
```bash
time-cli getblockchaininfo
```

### Check network connections
```bash
time-cli getnetworkinfo
```

---

## 🔗 Integration Examples

### Bash Script
```bash
#!/bin/bash

# Check if daemon is running
if time-cli uptime > /dev/null 2>&1; then
    echo "Daemon is running"
    UPTIME=$(time-cli uptime)
    echo "  Uptime: $UPTIME seconds"
else
    echo "Daemon is not running"
    exit 1
fi

# Get block count
BLOCKS=$(time-cli getblockcount)
echo "  Blocks: $BLOCKS"

# Get masternode count
MN_COUNT=$(time-cli masternodelist | jq '. | length')
echo "  Masternodes: $MN_COUNT"
```

### Python Script
```python
import subprocess
import json

def rpc_call(method):
    result = subprocess.run(
        ['time-cli', method],
        capture_output=True,
        text=True
    )
    return json.loads(result.stdout)

# Get blockchain info
info = rpc_call('getblockchaininfo')
print(f"Chain: {info['chain']}")
print(f"Blocks: {info['blocks']}")
print(f"Consensus: {info['consensus']}")
```

---

## 🚀 Help Command

```bash
time-cli --help
```

Shows all available commands and options.

```bash
time-cli <command> --help
```

Shows help for a specific command.

---

## 🎯 Comparison with Bitcoin CLI

### Commands identical to Bitcoin Core

| Command | Notes |
|---------|-------|
| `getblockchaininfo` | |
| `getblockcount` | |
| `getbestblockhash` | |
| `getblockhash` | |
| `getblockheader` | |
| `getblock` | |
| `getnetworkinfo` | |
| `getpeerinfo` | |
| `getconnectioncount` | |
| `getbanlist` | |
| `gettxoutsetinfo` | |
| `gettxout` | |
| `gettransaction` | |
| `getrawtransaction` | |
| `sendrawtransaction` | |
| `createrawtransaction` | |
| `decoderawtransaction` | |
| `estimatesmartfee` | |
| `listunspent` | |
| `getbalance` | |
| `getnewaddress` | |
| `getwalletinfo` | |
| `getaddressinfo` | |
| `listtransactions` | |
| `listreceivedbyaddress` | |
| `signmessage` | Ed25519 instead of ECDSA |
| `verifymessage` | Ed25519 instead of ECDSA |
| `validateaddress` | |
| `dumpprivkey` | Ed25519; runs offline, no daemon needed |
| `stop` | |
| `uptime` | |
| `getinfo` | Deprecated in Bitcoin Core; retained in TIME |
| `getmempoolinfo` | |
| `getrawmempool` | |

### Commands with different names than Bitcoin Core

| Bitcoin Core | TIME CLI | Notes |
|--------------|----------|-------|
| `setban <ip> add` | `ban <ip>` | Dedicated ban command |
| `setban <ip> remove` | `unban <ip>` | Dedicated unban |
| `setban <cidr> remove` | `unbansubnet <cidr>` | CIDR-aware unban |
| `clearbanned` | `clearbanlist` | |
| N/A | `aggregatebanlistss` | Multi-node ban list merger; no Bitcoin equivalent |
| `sendfrom` (deprecated) | `sendfrom` | Still active in TIME |
| `lockunspent false [...]` / `lockunspent true [...]` | `unlockutxo` / `listlockedutxos` | TIME uses dedicated lock commands |

### TIME-specific commands (no Bitcoin equivalent)

| Command | Purpose |
|---------|---------|
| `findblockbydate` | Binary-search chain for block nearest to a Unix timestamp |
| `getsupply` | Total / effectively-circulating supply |
| `getrewardreport` | Per-address and per-tier reward audit |
| `getconsensusinfo` | TimeVote consensus state |
| `gettimevotestatus` | Real-time TimeVote vote round status |
| `gettreasurybalance` | On-chain governance treasury balance |
| `getlocalwallet` | This node's wallet address |
| `listunspentmulti` | Bulk UTXO query for multiple addresses |
| `rebroadcasttransaction` | Re-announce a transaction to peers |
| `gettransactionfinality` | TimeVote finality status for a txid |
| `getmempoolverbose` | Full mempool detail including vote counts |
| `droptransaction` | Remove a transaction from local mempool |
| `clearstucktransactions` | Evict aged-out mempool transactions |
| `addwhitelist` / `getwhitelist` / `removewhitelist` | Trusted peer whitelist |
| `resetpeerprofiles` | Clear AI peer-scoring data |
| `masternode genkey` | Generate masternode Ed25519 key |
| `masternode list` / `masternodelist` | All registered masternodes |
| `masternode status` / `masternodestatus` | This node's masternode status |
| `masternodereg` | Sign and broadcast a registration transaction |
| `masternoderegstatus` | On-chain registration status |
| `auditcollateral` | Scan all nodes for invalid collateral |
| `checkcollateral` | Verify this node's collateral lock |
| `findcollateral` | Look up masternode by collateral outpoint |
| `listlockedcollaterals` | All active collateral locks |
| `releasecollateral` | Release a specific collateral lock + anchor |
| `releaseallcollaterals` | Release all collateral locks |
| `clearcollateralanchor` | Clear stale collateral anchor entry |
| `listlockedutxos` | All transaction UTXO locks |
| `unlockutxo` | Release a single transaction UTXO lock |
| `unlockcollateral` | Release a single collateral lock (in-memory only) |
| `unlockorphanedutxos` | Release locks for missing mempool txs |
| `forceunlockall` | Release all locks (emergency) |
| `cleanuplockedutxos` | Remove stale lock table entries |
| `reindex` | Rebuild UTXO set and tx index from genesis |
| `reindextransactions` | Rebuild tx index only (background) |
| `rollbacktoblock0` | Full chain reset to genesis |
| `rollbacktoheight` | Roll back to a specific height |
| `resyncfromwhitelist` | Re-download chain from trusted peers |
| `resetfinalitylock` | Clear BFT finality lock (fork recovery) |
| `gettxindexstatus` | Transaction index build status |
| `getfeeschedule` | Live tiered fee schedule |
| `listproposals` | Governance proposals |
| `getproposal` | Proposal detail and vote tally |
| `submitproposal` | Submit a governance proposal |
| `voteproposal` | Vote on a governance proposal |
| `request-payment` | Generate payment request URI |
| `pay-request` | Pay a payment request URI |
| `sendpaymentrequest` | Send a structured payment request to a peer |
| `getpaymentrequests` | Get pending payment requests |
| `respondpaymentrequest` | Accept or decline a payment request |
| `cancelpaymentrequest` | Cancel an outgoing payment request |
| `markpaymentrequestviewed` | Mark a payment request as viewed |
| `mergeutxos` | Consolidate UTXOs into one output |
| `sendfrom` | Send from a specific address |
| `aggregatebanlistss` | Merge ban lists from multiple nodes |
| `sendmessage` | Send an E2E encrypted message to a TIME address |
| `getmessages` | Fetch and decrypt incoming messages |
| `getmessagestatus` | Delivery/read-receipt status for a sent message |
| `getpubkey` | Resolve the Ed25519 messaging pubkey for an address |
| `addcontact` | Add or update a contact in the local contacts book |
| `listcontacts` | List all saved contacts |
| `removecontact` | Remove a contact from the contacts book |

---

## ⚙️ Advanced Usage

### Chaining Commands
```bash
# Get block count and save to file
time-cli getblockcount > block_height.txt

# Pretty print JSON
time-cli getblockchaininfo | jq .

# Extract specific field
time-cli getconsensusinfo | jq -r '.masternodes'
```

### Monitoring Script
```bash
#!/bin/bash
while true; do
    clear
    echo "=== TIME Coin Node Monitor ==="
    echo "Uptime:      $(time-cli uptime) seconds"
    echo "Blocks:      $(time-cli getblockcount)"
    echo "Peers:       $(time-cli getpeerinfo | jq 'length')"
    echo "Masternodes: $(time-cli masternodelist | jq 'length')"
    sleep 5
done
```

---

## 🔐 Security Notes

- RPC server listens on `127.0.0.1` by default (localhost only)
- For remote access, configure firewall rules carefully
- Consider using SSH tunneling for remote RPC access
- `dumpprivkey` runs offline and never contacts the daemon — keep output secure
- RPC authentication via `rpcuser`/`rpcpassword` in `time.conf` or cookie file

---

## 📝 Error Handling

### Connection Refused
```
Error: HTTP error: connection refused
```
**Solution**: Ensure `timed` daemon is running

### Method Not Found
```
Error: RPC error -32601: Method 'xyz' not found
```
**Solution**: Check command spelling or use `time-cli --help`

### Parse Error
```
Error: RPC error -32700: Parse error
```
**Solution**: Check JSON formatting in parameters

---

## 🎉 Features

- Bitcoin-compatible RPC interface
- Easy-to-use command-line interface
- JSON output for scripting
- Detailed error messages
- Human-readable output mode
- TIME-specific commands (consensus, masternodes, governance, UTXO lock management)

---

## 📚 See Also

- `START.md` - How to start the daemon
- `OPERATIONS.md` - Operations guide
- `MASTERNODE_GUIDE.md` - Masternode setup
- `GOVERNANCE.md` - On-chain governance
- `README.md` - Full documentation

---

```bash
cargo build --release
time-cli getblockchaininfo
```
