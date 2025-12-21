use crate::types::{OutPoint, UTXOState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Notification sent when a UTXO's state changes
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct StateChangeNotification {
    pub outpoint: OutPoint,
    pub old_state: Option<UTXOState>,
    pub new_state: UTXOState,
    pub timestamp: i64,
}

/// Pub/Sub system for real-time UTXO state change notifications
pub struct StateNotifier {
    /// Per-outpoint broadcast channels for subscribers
    subscribers: Arc<RwLock<HashMap<OutPoint, broadcast::Sender<StateChangeNotification>>>>,
    /// Broadcast channel for all state changes (for general subscribers)
    global_tx: broadcast::Sender<StateChangeNotification>,
}

impl StateNotifier {
    #[allow(dead_code)]
    pub fn new() -> Self {
        let (global_tx, _) = broadcast::channel(10_000);
        Self {
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            global_tx,
        }
    }

    /// Subscribe to a specific UTXO's state changes
    #[allow(dead_code)]
    pub async fn subscribe_to_outpoint(
        &self,
        outpoint: OutPoint,
    ) -> broadcast::Receiver<StateChangeNotification> {
        let mut subs = self.subscribers.write().await;
        let tx = subs
            .entry(outpoint)
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(1_000);
                tx
            })
            .clone();

        drop(subs);
        tx.subscribe()
    }

    /// Subscribe to all state changes globally
    #[allow(dead_code)]
    pub fn subscribe_globally(&self) -> broadcast::Receiver<StateChangeNotification> {
        self.global_tx.subscribe()
    }

    /// Notify subscribers of a state change
    pub async fn notify_state_change(
        &self,
        outpoint: OutPoint,
        old_state: Option<UTXOState>,
        new_state: UTXOState,
    ) {
        let notification = StateChangeNotification {
            outpoint: outpoint.clone(),
            old_state,
            new_state,
            timestamp: chrono::Utc::now().timestamp(),
        };

        // Send to specific outpoint subscribers
        if let Some(tx) = self.subscribers.read().await.get(&outpoint) {
            let _ = tx.send(notification.clone());
        }

        // Send to global subscribers
        let _ = self.global_tx.send(notification);
    }

    /// Check if there are subscribers for an outpoint
    #[allow(dead_code)]
    pub async fn has_subscribers(&self, outpoint: &OutPoint) -> bool {
        self.subscribers
            .read()
            .await
            .get(outpoint)
            .map(|tx| tx.receiver_count() > 0)
            .unwrap_or(false)
    }

    /// Get total subscriber count
    #[allow(dead_code)]
    pub async fn total_subscribers(&self) -> usize {
        self.subscribers
            .read()
            .await
            .values()
            .map(|tx| tx.receiver_count())
            .sum()
    }
}

impl Default for StateNotifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_subscribe_and_notify() {
        let notifier = StateNotifier::new();
        let outpoint = OutPoint {
            txid: [0u8; 32],
            vout: 0,
        };

        let mut rx = notifier.subscribe_to_outpoint(outpoint.clone()).await;

        notifier
            .notify_state_change(
                outpoint.clone(),
                Some(UTXOState::Unspent),
                UTXOState::Locked {
                    txid: [1u8; 32],
                    locked_at: 1000,
                },
            )
            .await;

        let notification = rx.recv().await.unwrap();
        assert_eq!(notification.outpoint, outpoint);
        assert_eq!(notification.timestamp, chrono::Utc::now().timestamp());
    }

    #[tokio::test]
    async fn test_global_subscribe() {
        let notifier = StateNotifier::new();
        let outpoint = OutPoint {
            txid: [0u8; 32],
            vout: 0,
        };

        let mut rx = notifier.subscribe_globally();

        notifier
            .notify_state_change(outpoint.clone(), None, UTXOState::Unspent)
            .await;

        let notification = rx.recv().await.unwrap();
        assert_eq!(notification.outpoint, outpoint);
    }
}
