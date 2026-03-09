//! In-memory Raft network transport for KKDB (Phase 1).
//!
//! Uses a shared registry of Raft handles so nodes in the same process
//! can communicate directly without real TCP sockets.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use openraft::{
    error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable},
    network::{RPCOption, RaftNetwork, RaftNetworkFactory},
    raft::{
        AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest,
        InstallSnapshotResponse, VoteRequest, VoteResponse,
    },
    BasicNode,
};

use crate::raft::types::{KkdbNodeId, KkdbTypeConfig};

/// A handle to a running Raft node for peer-to-peer RPC.
pub type NodeHandle = Arc<openraft::Raft<KkdbTypeConfig>>;

/// Shared registry: node_id → Raft handle.
pub type NodeRegistry = Arc<Mutex<BTreeMap<KkdbNodeId, NodeHandle>>>;

/// The network factory — one per node, holds registry reference.
#[derive(Clone)]
pub struct KkdbNetworkFactory {
    pub registry: NodeRegistry,
}

impl KkdbNetworkFactory {
    pub fn new(registry: NodeRegistry) -> Self {
        Self { registry }
    }
}

impl RaftNetworkFactory<KkdbTypeConfig> for KkdbNetworkFactory {
    type Network = KkdbNetwork;

    async fn new_client(&mut self, target: KkdbNodeId, _node: &BasicNode) -> Self::Network {
        KkdbNetwork {
            target,
            registry: Arc::clone(&self.registry),
        }
    }
}

/// Per-peer connection: routes RPC calls directly to the target's Raft handle.
pub struct KkdbNetwork {
    pub target: KkdbNodeId,
    pub registry: NodeRegistry,
}

impl KkdbNetwork {
    fn get_raft(&self) -> Result<NodeHandle, Unreachable> {
        self.registry
            .lock()
            .unwrap()
            .get(&self.target)
            .cloned()
            .ok_or_else(|| {
                let e = std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    format!("node {} not in registry", self.target),
                );
                Unreachable::new(&e)
            })
    }
}

impl RaftNetwork<KkdbTypeConfig> for KkdbNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<KkdbTypeConfig>,
        _option: RPCOption,
    ) -> Result<
        AppendEntriesResponse<KkdbNodeId>,
        RPCError<KkdbNodeId, BasicNode, RaftError<KkdbNodeId>>,
    > {
        let raft = self.get_raft().map_err(RPCError::Unreachable)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<KkdbNodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<KkdbNodeId>, RPCError<KkdbNodeId, BasicNode, RaftError<KkdbNodeId>>>
    {
        let raft = self.get_raft().map_err(RPCError::Unreachable)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<KkdbTypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<KkdbNodeId>,
        RPCError<KkdbNodeId, BasicNode, RaftError<KkdbNodeId, InstallSnapshotError>>,
    > {
        let raft = self.get_raft().map_err(RPCError::Unreachable)?;
        raft.install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
