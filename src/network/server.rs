use crate::consensus::ConsensusEngine;
use crate::network::message::{NetworkMessage, Subscription, UTXOStateChange};
use crate::network::rate_limiter::RateLimiter;
use crate::types::OutPoint;
use crate::utxo_manager::UTXOStateManager;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

#[allow(dead_code)]
pub struct NetworkServer {
    pub listener: TcpListener,
    pub peers: Arc<RwLock<HashMap<String, PeerConnection>>>,
    pub subscriptions: Arc<RwLock<HashMap<String, Subscription>>>,
    pub tx_notifier: broadcast::Sender<NetworkMessage>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub consensus: Arc<ConsensusEngine>,
    pub rate_limiter: Arc<RwLock<RateLimiter>>,
    pub masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
}

pub struct PeerConnection {
    pub addr: String,
    #[allow(dead_code)]
    pub is_masternode: bool,
}

impl NetworkServer {
    pub async fn new(
        bind_addr: &str,
        utxo_manager: Arc<UTXOStateManager>,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(bind_addr).await?;
        let (tx, _) = broadcast::channel(1024);

        Ok(Self {
            listener,
            peers: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            tx_notifier: tx,
            utxo_manager,
            consensus,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new())),
            masternode_registry: masternode_registry.clone(),
        })
    }

    pub async fn run(&mut self) -> Result<(), std::io::Error> {
        loop {
            let (stream, addr) = self.listener.accept().await?;
            let addr_str = addr.to_string();
            let peer = PeerConnection {
                addr: addr_str.clone(),
                is_masternode: false,
            };

            let peers = self.peers.clone();
            let subs = self.subscriptions.clone();
            let notifier = self.tx_notifier.subscribe();
            let utxo_mgr = self.utxo_manager.clone();
            let consensus = self.consensus.clone();
            let rate_limiter = self.rate_limiter.clone();
            let mn_registry = self.masternode_registry.clone();

            tokio::spawn(async move {
                let _ = handle_peer(
                    stream,
                    peer,
                    peers,
                    subs,
                    notifier,
                    utxo_mgr,
                    consensus,
                    rate_limiter,
                    mn_registry,
                )
                .await;
            });
        }
    }

    #[allow(dead_code)]
    pub async fn broadcast(&self, msg: NetworkMessage) {
        let _ = self.tx_notifier.send(msg);
    }

    #[allow(dead_code)]
    pub async fn notify_utxo_change(&self, outpoint: OutPoint, state: crate::types::UTXOState) {
        let change = UTXOStateChange {
            outpoint,
            new_state: state,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };
        self.broadcast(NetworkMessage::UTXOStateNotification(change))
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_peer(
    stream: TcpStream,
    peer: PeerConnection,
    _peers: Arc<RwLock<HashMap<String, PeerConnection>>>,
    subs: Arc<RwLock<HashMap<String, Subscription>>>,
    mut notifier: broadcast::Receiver<NetworkMessage>,
    utxo_mgr: Arc<UTXOStateManager>,
    consensus: Arc<ConsensusEngine>,
    rate_limiter: Arc<RwLock<RateLimiter>>,
    masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
) -> Result<(), std::io::Error> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);
    let mut line = String::new();

    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        tracing::debug!("📥 Received raw message from {}: {}", peer.addr, line.trim());
                        if let Ok(msg) = serde_json::from_str::<NetworkMessage>(&line) {
                            tracing::debug!("📦 Parsed message type from {}: {:?}", peer.addr, std::mem::discriminant(&msg));
                            let ip = &peer.addr;
                            let mut limiter = rate_limiter.write().await;

                            match &msg {
                                NetworkMessage::TransactionBroadcast(tx) => {
                                    if limiter.check("tx", ip) {
                                        if let Err(e) = consensus.process_transaction(tx.clone()).await {
                                            eprintln!("Tx rejected: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::UTXOStateQuery(outpoints) => {
                                    if limiter.check("utxo_query", ip) {
                                        let mut responses = Vec::new();
                                        for op in outpoints {
                                            if let Some(state) = utxo_mgr.get_state(op).await {
                                                responses.push((op.clone(), state));
                                            }
                                        }
                                        let reply = NetworkMessage::UTXOStateResponse(responses);
                                        if let Ok(json) = serde_json::to_string(&reply) {
                                            let _ = writer.write_all(json.as_bytes()).await;
                                            let _ = writer.write_all(b"\n").await;
                                            let _ = writer.flush().await;
                                        }
                                    }
                                }
                                NetworkMessage::Subscribe(sub) => {
                                    if limiter.check("subscribe", ip) {
                                        subs.write().await.insert(sub.id.clone(), sub.clone());
                                    }
                                }
                                NetworkMessage::MasternodeAnnouncement { address: _, reward_address, tier, public_key } => {
                                    // Use the peer's actual connection address, not the announced address
                                    let actual_address = peer.addr.clone();
                                    tracing::info!("📨 Received masternode announcement from {}", actual_address);
                                    let mn = crate::types::Masternode {
                                        address: actual_address,
                                        wallet_address: reward_address.clone(),
                                        collateral: tier.collateral(),
                                        tier: tier.clone(),
                                        public_key: *public_key,
                                        registered_at: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs(),
                                    };

                                    if let Err(e) = masternode_registry.register(
                                        mn,
                                        reward_address.clone()
                                    ).await {
                                        tracing::warn!("Failed to register masternode: {}", e);
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            tracing::warn!("❌ Failed to parse message from {}: {}", peer.addr, line.trim());
                        }
                        line.clear();
                    }
                    Err(e) => {
                        tracing::warn!("❌ Read error from {}: {}", peer.addr, e);
                        break;
                    }
                }
            }

            result = notifier.recv() => {
                match result {
                    Ok(msg) => {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = writer.write_all(json.as_bytes()).await;
                            let _ = writer.write_all(b"\n").await;
                            let _ = writer.flush().await;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    Ok(())
}
