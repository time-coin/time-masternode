use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Represents the state of a peer connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected, no attempt in progress
    Disconnected,
    /// Attempting to establish connection
    Connecting { started_at: Instant },
    /// Successfully connected
    Connected { since: Instant },
    /// Connection failed, waiting before retry
    Reconnecting {
        backoff_until: Instant,
        attempt: u32,
    },
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected { .. })
    }

    pub fn is_connecting(&self) -> bool {
        matches!(self, ConnectionState::Connecting { .. })
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self, ConnectionState::Disconnected)
    }

    pub fn attempt_number(&self) -> u32 {
        match self {
            ConnectionState::Reconnecting { attempt, .. } => *attempt,
            _ => 0,
        }
    }
}

/// Connection state machine that prevents race conditions
/// and ensures deterministic connection state management
pub struct ConnectionStateMachine {
    states: Arc<RwLock<HashMap<String, ConnectionState>>>,
}

impl ConnectionStateMachine {
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get current state of a peer
    pub async fn get_state(&self, peer_ip: &str) -> ConnectionState {
        self.states
            .read()
            .await
            .get(peer_ip)
            .copied()
            .unwrap_or(ConnectionState::Disconnected)
    }

    /// Try to transition from current state to new state
    /// Returns true if transition succeeded, false if it was invalid
    pub async fn try_transition(&self, peer_ip: &str, new_state: ConnectionState) -> bool {
        let mut states = self.states.write().await;
        let current = states.get(peer_ip).copied();

        // Validate state transitions
        let valid = match (current, new_state) {
            // From Disconnected, can only go to Connecting
            (None, ConnectionState::Connecting { .. }) => true,
            (Some(ConnectionState::Disconnected), ConnectionState::Connecting { .. }) => true,

            // From Connecting, can go to Connected or Reconnecting
            (Some(ConnectionState::Connecting { .. }), ConnectionState::Connected { .. }) => true,
            (Some(ConnectionState::Connecting { .. }), ConnectionState::Reconnecting { .. }) => {
                true
            }

            // From Connected, can go to Reconnecting or Disconnected
            (Some(ConnectionState::Connected { .. }), ConnectionState::Reconnecting { .. }) => true,
            (Some(ConnectionState::Connected { .. }), ConnectionState::Disconnected) => true,

            // From Reconnecting, can go to Connecting or Disconnected
            (Some(ConnectionState::Reconnecting { .. }), ConnectionState::Connecting { .. }) => {
                true
            }
            (Some(ConnectionState::Reconnecting { .. }), ConnectionState::Disconnected) => true,

            // Invalid transitions
            _ => false,
        };

        if valid {
            states.insert(peer_ip.to_string(), new_state);
            debug!(
                "✅ State transition for {}: {:?} -> {:?}",
                peer_ip, current, new_state
            );
            true
        } else {
            warn!(
                "❌ Invalid state transition for {}: {:?} -> {:?}",
                peer_ip, current, new_state
            );
            false
        }
    }

    /// Helper: Try to mark as connecting
    pub async fn mark_connecting(&self, peer_ip: &str) -> bool {
        self.try_transition(
            peer_ip,
            ConnectionState::Connecting {
                started_at: Instant::now(),
            },
        )
        .await
    }

    /// Helper: Mark as connected
    pub async fn mark_connected(&self, peer_ip: &str) -> bool {
        self.try_transition(
            peer_ip,
            ConnectionState::Connected {
                since: Instant::now(),
            },
        )
        .await
    }

    /// Helper: Mark as reconnecting with exponential backoff
    pub async fn mark_reconnecting(&self, peer_ip: &str) -> bool {
        let state = self.get_state(peer_ip).await;
        let attempt = state.attempt_number() + 1;

        // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 32s (max)
        let backoff_secs = (2u64.pow(attempt.saturating_sub(1).min(5))) as u64;
        let backoff_until = Instant::now() + std::time::Duration::from_secs(backoff_secs);

        self.try_transition(
            peer_ip,
            ConnectionState::Reconnecting {
                backoff_until,
                attempt,
            },
        )
        .await
    }

    /// Helper: Mark as disconnected
    pub async fn mark_disconnected(&self, peer_ip: &str) -> bool {
        self.try_transition(peer_ip, ConnectionState::Disconnected)
            .await
    }

    /// Check if peer is ready to reconnect (backoff expired)
    pub async fn is_ready_to_reconnect(&self, peer_ip: &str) -> bool {
        match self.get_state(peer_ip).await {
            ConnectionState::Reconnecting { backoff_until, .. } => Instant::now() >= backoff_until,
            _ => false,
        }
    }

    /// Get list of all peers currently connected
    pub async fn get_connected_peers(&self) -> Vec<String> {
        let states = self.states.read().await;
        states
            .iter()
            .filter(|(_, state)| state.is_connected())
            .map(|(ip, _)| ip.clone())
            .collect()
    }

    /// Get list of all peers in connecting state
    pub async fn get_connecting_peers(&self) -> Vec<String> {
        let states = self.states.read().await;
        states
            .iter()
            .filter(|(_, state)| state.is_connecting())
            .map(|(ip, _)| ip.clone())
            .collect()
    }

    /// Remove a peer's state (for cleanup)
    pub async fn remove_peer(&self, peer_ip: &str) {
        self.states.write().await.remove(peer_ip);
    }

    /// Get statistics about connection states
    pub async fn get_stats(&self) -> ConnectionStats {
        let states = self.states.read().await;
        let mut stats = ConnectionStats::default();

        for state in states.values() {
            match state {
                ConnectionState::Disconnected => stats.disconnected += 1,
                ConnectionState::Connecting { .. } => stats.connecting += 1,
                ConnectionState::Connected { .. } => stats.connected += 1,
                ConnectionState::Reconnecting { .. } => stats.reconnecting += 1,
            }
        }

        stats.total = states.len();
        stats
    }
}

impl Default for ConnectionStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub total: usize,
    pub disconnected: usize,
    pub connecting: usize,
    pub connected: usize,
    pub reconnecting: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_valid_transitions() {
        let machine = ConnectionStateMachine::new();

        // Disconnected -> Connecting
        assert!(machine.mark_connecting("peer1").await);

        // Connecting -> Connected
        assert!(
            machine
                .try_transition(
                    "peer1",
                    ConnectionState::Connected {
                        since: Instant::now()
                    }
                )
                .await
        );

        // Connected -> Reconnecting
        assert!(machine.mark_reconnecting("peer1").await);

        // Reconnecting -> Connecting
        assert!(machine.mark_connecting("peer1").await);
    }

    #[tokio::test]
    async fn test_invalid_transitions() {
        let machine = ConnectionStateMachine::new();

        // Try to go from Disconnected directly to Connected (invalid)
        assert!(
            !machine
                .try_transition(
                    "peer1",
                    ConnectionState::Connected {
                        since: Instant::now()
                    }
                )
                .await
        );

        // Verify state is still Disconnected
        assert_eq!(
            machine.get_state("peer1").await,
            ConnectionState::Disconnected
        );
    }

    #[tokio::test]
    async fn test_exponential_backoff() {
        let machine = ConnectionStateMachine::new();

        machine.mark_connecting("peer1").await;
        machine
            .try_transition(
                "peer1",
                ConnectionState::Connected {
                    since: Instant::now(),
                },
            )
            .await;

        // First reconnect attempt
        machine.mark_reconnecting("peer1").await;
        assert_eq!(machine.get_state("peer1").await.attempt_number(), 1);

        // Verify not ready to reconnect immediately
        assert!(!machine.is_ready_to_reconnect("peer1").await);

        // Second attempt (would have longer backoff)
        machine.mark_reconnecting("peer1").await;
        assert_eq!(machine.get_state("peer1").await.attempt_number(), 2);
    }

    #[tokio::test]
    async fn test_get_connected_peers() {
        let machine = ConnectionStateMachine::new();

        // Create some peers in different states
        machine.mark_connecting("peer1").await;
        machine
            .try_transition(
                "peer1",
                ConnectionState::Connected {
                    since: Instant::now(),
                },
            )
            .await;
        machine.mark_connecting("peer2").await;

        let connected_peers = machine.get_connected_peers().await;
        assert_eq!(connected_peers.len(), 1);
        assert!(connected_peers.contains(&"peer1".to_string()));

        let connecting_peers = machine.get_connecting_peers().await;
        assert_eq!(connecting_peers.len(), 1);
        assert!(connecting_peers.contains(&"peer2".to_string()));
    }
}
