use crate::masternode_registry::MasternodeRegistry;
use crate::network::message::NetworkMessage;
use crate::peer_manager::PeerManager;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};

pub struct NetworkClient {
    peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
}

impl NetworkClient {
    pub fn new(
        peer_manager: Arc<PeerManager>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Self {
        Self {
            peer_manager,
            masternode_registry,
        }
    }

    /// Start connecting to peers and announcing our masternode
    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();

        tokio::spawn(async move {
            loop {
                // Get list of known peers
                let peers = peer_manager.get_all_peers().await;

                for peer_addr in peers {
                    let pm = peer_manager.clone();
                    let mr = masternode_registry.clone();

                    tokio::spawn(async move {
                        if let Err(e) = connect_to_peer(&peer_addr, pm, mr).await {
                            tracing::debug!("Failed to connect to {}: {}", peer_addr, e);
                        }
                    });
                }

                // Try to connect to peers every 30 seconds
                sleep(Duration::from_secs(30)).await;
            }
        });
    }
}

async fn connect_to_peer(
    address: &str,
    _peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
) -> Result<(), String> {
    // Try to connect
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    tracing::info!("✓ Connected to peer: {}", address);

    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    // Announce our masternode if we are one
    if let Some(local_mn) = masternode_registry.get_local_masternode().await {
        let announce_msg = NetworkMessage::MasternodeAnnouncement {
            address: local_mn.masternode.address.clone(),
            reward_address: local_mn.reward_address.clone(),
            tier: local_mn.masternode.tier,
            public_key: local_mn.masternode.public_key,
        };

        let msg_json = serde_json::to_string(&announce_msg)
            .map_err(|e| format!("Failed to serialize: {}", e))?;

        writer
            .write_all(format!("{}\n", msg_json).as_bytes())
            .await
            .map_err(|e| format!("Write failed: {}", e))?;
        writer
            .flush()
            .await
            .map_err(|e| format!("Flush failed: {}", e))?;

        tracing::info!("📡 Announced masternode to {}", address);
    }

    // Read responses
    let mut line = String::new();
    while let Ok(n) = reader.read_line(&mut line).await {
        if n == 0 {
            break;
        }

        if let Ok(NetworkMessage::MasternodeAnnouncement {
            address: mn_addr,
            reward_address,
            tier,
            public_key,
        }) = serde_json::from_str::<NetworkMessage>(&line)
        {
            // Register the remote masternode
            if let Err(e) = masternode_registry
                .register_masternode(mn_addr.clone(), reward_address, tier, public_key)
                .await
            {
                tracing::warn!("Failed to register masternode {}: {}", mn_addr, e);
            } else {
                tracing::info!("✓ Registered remote masternode: {}", mn_addr);
            }
        }

        line.clear();
    }

    Ok(())
}
