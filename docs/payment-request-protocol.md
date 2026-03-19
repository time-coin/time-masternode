# Payment Request Protocol

All payment request RPC calls accept a **JSON object** as `params[0]`.
Amounts are always **u64 satoshis** (1 TIME = 100,000,000 satoshis).

---

## RPC Methods

### `sendpaymentrequest`
Requester wallet → masternode: store and relay a payment request to the payer.

```json
{
  "id":                "uuid-string",       // optional; computed from hash if absent
  "requester_address": "TIME1...",          // required — who wants to be paid
  "payer_address":     "TIME1...",          // required — who is being asked to pay
  "amount":            100000000,           // required — u64 satoshis
  "memo":              "Invoice #42",       // optional
  "requester_name":    "Alice",             // optional — display name
  "pubkey_hex":        "aabbcc...",         // optional — Ed25519 pubkey (32 bytes hex)
  "signature_hex":     "aabbcc...",         // optional — Ed25519 sig (64 bytes hex)
  "timestamp":         1700000000           // optional — Unix timestamp; defaults to now
}
```

Response: `{ "id": "...", "status": "sent", "expires": 1700086400 }`

Signature (when present) is verified over:
`id || requester_address || payer_address || amount_le8 || memo || timestamp_le8`

---

### `respondpaymentrequest`
Payer wallet → masternode: accept or decline a pending request.

```json
{
  "id":           "request-id",   // required
  "payer_address": "TIME1...",    // required
  "accepted":     true,           // required — boolean
  "txid":         "aabbcc..."     // optional — txid if accepted and already paid
}
```

Response: `{ "id": "...", "accepted": true, "status": "accepted" }`

---

### `cancelpaymentrequest`
Requester wallet → masternode: cancel a pending outgoing request.

```json
{
  "id":                "request-id",   // required
  "requester_address": "TIME1..."      // required
}
```

Response: `{ "id": "...", "status": "cancelled", "removed": true }`

---

### `markpaymentrequestviewed`
Payer wallet → masternode: notify requester their request was opened.

```json
{
  "id":           "request-id",   // required
  "payer_address": "TIME1..."     // required
}
```

Response: `{ "id": "...", "status": "viewed" }`

---

## WebSocket Events

### `payment_request`
Pushed to the payer's subscribed connection when a new request arrives.

```json
{
  "requester_address": "TIME1...",
  "payer_address":     "TIME1...",
  "amount":            1.0,
  "memo":              "Invoice #42",
  "requester_name":    "Alice",
  "pubkey":            "aabbcc...",
  "timestamp":         1700000000,
  "expires":           1700086400
}
```

`amount` in the WS event is a **float BTC** value (standard for WS outputs).

### `payment_request_response`
Pushed to the requester when the payer responds.

```json
{ "id": "...", "payer_address": "TIME1...", "accepted": true, "txid": null }
```

### `payment_request_cancelled`
Pushed to the payer when the requester cancels.

```json
{ "id": "...", "requester_address": "TIME1..." }
```

### `payment_request_viewed`
Pushed to the requester when the payer opens the request.

```json
{ "id": "...", "payer_address": "TIME1..." }
```
