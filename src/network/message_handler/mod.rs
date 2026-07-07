//! Unified message handler for both inbound and outbound connections.
//!
//! Domain handlers live in submodules; `core` owns dispatch and shared helpers.

mod blocks;
mod common;
mod consensus;
mod context;
mod core;
mod health;
mod masternode;
mod peers;
mod transactions;
mod utxo;

pub use crate::network::connection_direction::ConnectionDirection;
pub use common::probe_masternode_reachability;
pub use context::MessageContext;
pub use core::MessageHandler;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_direction_display() {
        assert_eq!(format!("{}", ConnectionDirection::Inbound), "Inbound");
        assert_eq!(format!("{}", ConnectionDirection::Outbound), "Outbound");
    }

    #[test]
    fn test_message_handler_new() {
        let _handler = MessageHandler::new("127.0.0.1".to_string(), ConnectionDirection::Inbound);
    }
}
