//! HTTP-based Raft network client for KKDB (Phase 2, cross-process).
//!
//! `HttpNetworkFactory` implements `RaftNetworkFactory` using `reqwest` to
//! POST JSON to the target node's `/raft/*` endpoints.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openraft::{
    error::{InstallSnapshotError, RPCError, RaftError, Unreachable},
    network::{RPCOption, RaftNetwork, RaftNetworkFactory},
    raft::{
        AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest,
        InstallSnapshotResponse, VoteRequest, VoteResponse,
    },
    BasicNode,
};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::raft::types::{KkdbNodeId, KkdbTypeConfig};

// ─── Address registry ─────────────────────────────────────────────────────────

/// Maps each node_id → that node's Raft HTTP base URL (e.g. "http://127.0.0.1:7001")
pub type AddrRegistry = Arc<Mutex<BTreeMap<KkdbNodeId, String>>>;

// ─── Factory ──────────────────────────────────────────────────────────────────

/// HTTP-based Raft network factory.
#[derive(Clone)]
pub struct HttpNetworkFactory {
    pub addresses: AddrRegistry,
    pub client: reqwest::Client,
}

impl HttpNetworkFactory {
    /// `addresses` maps node_id → "http://host:port" for all known peers.
    pub fn new(addresses: AddrRegistry) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build reqwest client");
        Self { addresses, client }
    }
}

impl RaftNetworkFactory<KkdbTypeConfig> for HttpNetworkFactory {
    type Network = HttpNetwork;

    async fn new_client(&mut self, target: KkdbNodeId, node: &BasicNode) -> Self::Network {
        // Prefer the address from our registry; fall back to node.addr
        let base = {
            let reg = self.addresses.lock().unwrap();
            reg.get(&target)
                .cloned()
                .unwrap_or_else(|| format!("http://{}", node.addr))
        };
        HttpNetwork {
            target,
            base_url: base,
            client: self.client.clone(),
        }
    }
}

// ─── Per-peer HTTP connection ─────────────────────────────────────────────────

/// One HTTP connection to a single peer node.
pub struct HttpNetwork {
    pub target: KkdbNodeId,
    pub base_url: String,
    pub client: reqwest::Client,
}

impl HttpNetwork {
    /// POST `body` to `{base_url}{path}` and deserialize `R` from the response.
    async fn post<B, R>(&self, path: &str, body: &B) -> Result<R, Unreachable>
    where
        B: Serialize,
        R: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                Unreachable::new(&std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    e.to_string(),
                ))
            })?;

        resp.json::<R>().await.map_err(|e| {
            Unreachable::new(&std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            ))
        })
    }
}

impl RaftNetwork<KkdbTypeConfig> for HttpNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<KkdbTypeConfig>,
        _option: RPCOption,
    ) -> Result<
        AppendEntriesResponse<KkdbNodeId>,
        RPCError<KkdbNodeId, BasicNode, RaftError<KkdbNodeId>>,
    > {
        self.post("/raft/append-entries", &rpc)
            .await
            .map_err(RPCError::Unreachable)
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<KkdbNodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<KkdbNodeId>, RPCError<KkdbNodeId, BasicNode, RaftError<KkdbNodeId>>>
    {
        self.post("/raft/vote", &rpc)
            .await
            .map_err(RPCError::Unreachable)
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<KkdbTypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<KkdbNodeId>,
        RPCError<KkdbNodeId, BasicNode, RaftError<KkdbNodeId, InstallSnapshotError>>,
    > {
        self.post("/raft/install-snapshot", &rpc)
            .await
            .map_err(RPCError::Unreachable)
    }
}
