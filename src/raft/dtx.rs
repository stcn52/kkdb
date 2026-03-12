//! Distributed Transaction Coordinator — Two-Phase Commit (2PC) Protocol
//!
//! Implements the classic 2PC protocol for cross-node atomic transactions
//! on top of the existing Raft consensus layer.
//!
//! # Protocol Flow
//!
//! ```text
//!  Coordinator                   Participant A           Participant B
//!      │                              │                       │
//!      │── PREPARE ──────────────────►│                       │
//!      │── PREPARE ──────────────────────────────────────────►│
//!      │                              │                       │
//!      │◄─ VOTE_COMMIT ──────────────│                       │
//!      │◄─ VOTE_COMMIT ──────────────────────────────────────│
//!      │                              │                       │
//!      │── COMMIT ───────────────────►│                       │
//!      │── COMMIT ───────────────────────────────────────────►│
//!      │                              │                       │
//!      │◄─ ACK ──────────────────────│                       │
//!      │◄─ ACK ──────────────────────────────────────────────│
//! ```
//!
//! # 3PC Extension
//!
//! Adds a PRE_COMMIT phase between PREPARE and COMMIT to avoid blocking
//! when the coordinator fails after collecting all votes:
//!
//! ```text
//!  PREPARE → VOTE → PRE_COMMIT → ACK → COMMIT → ACK
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Globally unique transaction ID for distributed transactions.
pub type DtxId = u64;

/// Participant node identifier.
pub type ParticipantId = u64;

/// State of a distributed transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtxState {
    /// Transaction initiated, preparing to send PREPARE.
    Init,
    /// PREPARE messages sent, waiting for votes.
    Preparing,
    /// All participants voted COMMIT — pre-commit phase (3PC only).
    PreCommitted,
    /// Decision: COMMIT — sending COMMIT to all participants.
    Committing,
    /// Decision: ABORT — sending ABORT to all participants.
    Aborting,
    /// Terminal: successfully committed on all participants.
    Committed,
    /// Terminal: aborted on all participants.
    Aborted,
}

impl fmt::Display for DtxState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DtxState::Init => write!(f, "INIT"),
            DtxState::Preparing => write!(f, "PREPARING"),
            DtxState::PreCommitted => write!(f, "PRE_COMMITTED"),
            DtxState::Committing => write!(f, "COMMITTING"),
            DtxState::Aborting => write!(f, "ABORTING"),
            DtxState::Committed => write!(f, "COMMITTED"),
            DtxState::Aborted => write!(f, "ABORTED"),
        }
    }
}

/// Vote from a participant in phase 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    /// Participant is ready to commit.
    Commit,
    /// Participant cannot commit (constraint violation, timeout, etc.).
    Abort,
}

/// A message in the 2PC/3PC protocol.
#[derive(Debug, Clone)]
pub enum DtxMessage {
    /// Phase 1: Coordinator asks participant to prepare.
    Prepare {
        dtx_id: DtxId,
        sql: String,
    },
    /// Phase 1 response: Participant votes.
    VoteResult {
        dtx_id: DtxId,
        participant: ParticipantId,
        vote: Vote,
        /// Optional error message if vote is Abort.
        reason: Option<String>,
    },
    /// Phase 2 (3PC only): Coordinator signals pre-commit.
    PreCommit {
        dtx_id: DtxId,
    },
    /// Phase 2 (3PC only): Participant acknowledges pre-commit.
    PreCommitAck {
        dtx_id: DtxId,
        participant: ParticipantId,
    },
    /// Final phase: Coordinator orders commit.
    Commit {
        dtx_id: DtxId,
    },
    /// Final phase: Coordinator orders abort.
    Abort {
        dtx_id: DtxId,
        reason: String,
    },
    /// Acknowledgement from participant after commit/abort.
    Ack {
        dtx_id: DtxId,
        participant: ParticipantId,
        success: bool,
    },
}

/// A single participant's state in a distributed transaction.
#[derive(Debug, Clone)]
pub struct ParticipantState {
    pub id: ParticipantId,
    pub vote: Option<Vote>,
    pub vote_reason: Option<String>,
    pub pre_commit_acked: bool,
    pub committed: bool,
    pub aborted: bool,
}

impl ParticipantState {
    fn new(id: ParticipantId) -> Self {
        Self {
            id,
            vote: None,
            vote_reason: None,
            pre_commit_acked: false,
            committed: false,
            aborted: false,
        }
    }
}

/// Coordinator-side view of a distributed transaction.
#[derive(Debug, Clone)]
pub struct DistributedTransaction {
    /// Unique transaction ID.
    pub id: DtxId,
    /// SQL statement(s) being executed across participants.
    pub sql: String,
    /// Current state of the 2PC protocol.
    pub state: DtxState,
    /// Per-participant state.
    pub participants: HashMap<ParticipantId, ParticipantState>,
    /// Whether to use 3PC (with pre-commit phase).
    pub use_3pc: bool,
    /// Timeout for each phase.
    pub phase_timeout: Duration,
    /// When the current phase started.
    pub phase_start: Instant,
    /// Decision log for recovery.
    pub decision_log: Vec<String>,
}

impl DistributedTransaction {
    /// Create a new distributed transaction.
    pub fn new(
        id: DtxId,
        sql: String,
        participants: Vec<ParticipantId>,
        use_3pc: bool,
        phase_timeout: Duration,
    ) -> Self {
        let participant_map = participants
            .into_iter()
            .map(|pid| (pid, ParticipantState::new(pid)))
            .collect();
        Self {
            id,
            sql,
            state: DtxState::Init,
            participants: participant_map,
            use_3pc,
            phase_timeout,
            phase_start: Instant::now(),
            decision_log: vec![format!("DTX-{}: created", id)],
        }
    }

    /// Number of participants.
    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    /// Check if we have all votes collected.
    pub fn all_votes_received(&self) -> bool {
        self.participants.values().all(|p| p.vote.is_some())
    }

    /// Check if all votes are COMMIT.
    pub fn all_voted_commit(&self) -> bool {
        self.participants
            .values()
            .all(|p| p.vote == Some(Vote::Commit))
    }

    /// Check if any participant voted ABORT.
    pub fn any_voted_abort(&self) -> bool {
        self.participants
            .values()
            .any(|p| p.vote == Some(Vote::Abort))
    }

    /// Check if all pre-commit acks received (3PC).
    pub fn all_pre_commit_acked(&self) -> bool {
        self.participants.values().all(|p| p.pre_commit_acked)
    }

    /// Check if all participants have committed.
    pub fn all_committed(&self) -> bool {
        self.participants.values().all(|p| p.committed)
    }

    /// Check if all participants have aborted.
    pub fn all_aborted(&self) -> bool {
        self.participants.values().all(|p| p.aborted)
    }

    /// Check if the current phase has timed out.
    pub fn phase_timed_out(&self) -> bool {
        self.phase_start.elapsed() > self.phase_timeout
    }

    /// Record a vote from a participant.
    pub fn record_vote(
        &mut self,
        participant: ParticipantId,
        vote: Vote,
        reason: Option<String>,
    ) -> Result<(), String> {
        let ps = self
            .participants
            .get_mut(&participant)
            .ok_or_else(|| format!("unknown participant {}", participant))?;
        if ps.vote.is_some() {
            return Err(format!(
                "participant {} already voted",
                participant
            ));
        }
        ps.vote = Some(vote);
        ps.vote_reason = reason.clone();
        self.decision_log.push(format!(
            "DTX-{}: participant {} voted {:?}{}",
            self.id,
            participant,
            vote,
            reason.map(|r| format!(" ({})", r)).unwrap_or_default()
        ));
        Ok(())
    }

    /// Record a pre-commit ack from a participant (3PC).
    pub fn record_pre_commit_ack(&mut self, participant: ParticipantId) -> Result<(), String> {
        let ps = self
            .participants
            .get_mut(&participant)
            .ok_or_else(|| format!("unknown participant {}", participant))?;
        ps.pre_commit_acked = true;
        self.decision_log
            .push(format!("DTX-{}: participant {} pre-commit-acked", self.id, participant));
        Ok(())
    }

    /// Record that a participant has committed.
    pub fn record_committed(&mut self, participant: ParticipantId) -> Result<(), String> {
        let ps = self
            .participants
            .get_mut(&participant)
            .ok_or_else(|| format!("unknown participant {}", participant))?;
        ps.committed = true;
        self.decision_log
            .push(format!("DTX-{}: participant {} committed", self.id, participant));
        Ok(())
    }

    /// Record that a participant has aborted.
    pub fn record_aborted(&mut self, participant: ParticipantId) -> Result<(), String> {
        let ps = self
            .participants
            .get_mut(&participant)
            .ok_or_else(|| format!("unknown participant {}", participant))?;
        ps.aborted = true;
        self.decision_log
            .push(format!("DTX-{}: participant {} aborted", self.id, participant));
        Ok(())
    }
}

/// The 2PC/3PC Coordinator — manages distributed transaction state.
///
/// Thread-safe via inner `Mutex`.
pub struct DtxCoordinator {
    inner: Mutex<CoordinatorInner>,
}

struct CoordinatorInner {
    /// Active distributed transactions.
    transactions: HashMap<DtxId, DistributedTransaction>,
    /// Next transaction ID.
    next_id: DtxId,
    /// Completed transaction log (last N for debugging).
    completed_log: Vec<(DtxId, DtxState)>,
    /// Maximum completed log entries to retain.
    max_completed_log: usize,
    /// Statistics.
    stats: DtxStats,
}

/// Coordinator statistics.
#[derive(Debug, Clone, Default)]
pub struct DtxStats {
    pub total_transactions: u64,
    pub total_committed: u64,
    pub total_aborted: u64,
    pub total_timeouts: u64,
    pub active_transactions: usize,
}

impl DtxCoordinator {
    /// Create a new coordinator.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CoordinatorInner {
                transactions: HashMap::new(),
                next_id: 1,
                completed_log: Vec::new(),
                max_completed_log: 100,
                stats: DtxStats::default(),
            }),
        }
    }

    /// Begin a new distributed transaction.
    ///
    /// Returns the `DtxId` and a list of `DtxMessage::Prepare` messages
    /// that should be sent to each participant.
    pub fn begin(
        &self,
        sql: String,
        participants: Vec<ParticipantId>,
        use_3pc: bool,
        phase_timeout: Duration,
    ) -> (DtxId, Vec<DtxMessage>) {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.stats.total_transactions += 1;
        inner.stats.active_transactions += 1;

        let dtx = DistributedTransaction::new(
            id,
            sql.clone(),
            participants.clone(),
            use_3pc,
            phase_timeout,
        );
        inner.transactions.insert(id, dtx);

        let messages = participants
            .into_iter()
            .map(|_pid| DtxMessage::Prepare {
                dtx_id: id,
                sql: sql.clone(),
            })
            .collect();

        (id, messages)
    }

    /// Process a vote from a participant.
    ///
    /// Returns the next messages to send (if any), or the final decision.
    pub fn process_vote(
        &self,
        dtx_id: DtxId,
        participant: ParticipantId,
        vote: Vote,
        reason: Option<String>,
    ) -> Result<Vec<DtxMessage>, String> {
        let mut inner = self.inner.lock().unwrap();
        let dtx = inner
            .transactions
            .get_mut(&dtx_id)
            .ok_or_else(|| format!("DTX-{} not found", dtx_id))?;

        dtx.record_vote(participant, vote, reason)?;

        // If any participant votes ABORT, immediately decide ABORT
        if dtx.any_voted_abort() {
            dtx.state = DtxState::Aborting;
            let abort_reason = dtx
                .participants
                .values()
                .filter_map(|p| {
                    if p.vote == Some(Vote::Abort) {
                        p.vote_reason.clone()
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            dtx.decision_log
                .push(format!("DTX-{}: decision=ABORT ({})", dtx_id, abort_reason));
            return Ok(vec![DtxMessage::Abort {
                dtx_id,
                reason: abort_reason,
            }]);
        }

        // If all votes received and all are COMMIT
        if dtx.all_votes_received() && dtx.all_voted_commit() {
            if dtx.use_3pc {
                // 3PC: move to PRE_COMMIT phase
                dtx.state = DtxState::PreCommitted;
                dtx.phase_start = Instant::now();
                dtx.decision_log
                    .push(format!("DTX-{}: all voted COMMIT, entering PRE_COMMIT", dtx_id));
                return Ok(vec![DtxMessage::PreCommit { dtx_id }]);
            } else {
                // 2PC: directly COMMIT
                dtx.state = DtxState::Committing;
                dtx.phase_start = Instant::now();
                dtx.decision_log
                    .push(format!("DTX-{}: all voted COMMIT, decision=COMMIT", dtx_id));
                return Ok(vec![DtxMessage::Commit { dtx_id }]);
            }
        }

        // Still waiting for more votes
        Ok(vec![])
    }

    /// Process a pre-commit acknowledgement (3PC only).
    pub fn process_pre_commit_ack(
        &self,
        dtx_id: DtxId,
        participant: ParticipantId,
    ) -> Result<Vec<DtxMessage>, String> {
        let mut inner = self.inner.lock().unwrap();
        let dtx = inner
            .transactions
            .get_mut(&dtx_id)
            .ok_or_else(|| format!("DTX-{} not found", dtx_id))?;

        dtx.record_pre_commit_ack(participant)?;

        if dtx.all_pre_commit_acked() {
            dtx.state = DtxState::Committing;
            dtx.phase_start = Instant::now();
            dtx.decision_log
                .push(format!("DTX-{}: all pre-commit acked, decision=COMMIT", dtx_id));
            return Ok(vec![DtxMessage::Commit { dtx_id }]);
        }

        Ok(vec![])
    }

    /// Process a commit/abort acknowledgement from a participant.
    pub fn process_ack(
        &self,
        dtx_id: DtxId,
        participant: ParticipantId,
        was_commit: bool,
    ) -> Result<Option<DtxState>, String> {
        let mut inner = self.inner.lock().unwrap();
        let dtx = inner
            .transactions
            .get_mut(&dtx_id)
            .ok_or_else(|| format!("DTX-{} not found", dtx_id))?;

        if was_commit {
            dtx.record_committed(participant)?;
            if dtx.all_committed() {
                dtx.state = DtxState::Committed;
                inner.stats.total_committed += 1;
                inner.stats.active_transactions =
                    inner.stats.active_transactions.saturating_sub(1);
                inner
                    .completed_log
                    .push((dtx_id, DtxState::Committed));
                if inner.completed_log.len() > inner.max_completed_log {
                    inner.completed_log.remove(0);
                }
                return Ok(Some(DtxState::Committed));
            }
        } else {
            dtx.record_aborted(participant)?;
            if dtx.all_aborted() {
                dtx.state = DtxState::Aborted;
                inner.stats.total_aborted += 1;
                inner.stats.active_transactions =
                    inner.stats.active_transactions.saturating_sub(1);
                inner.completed_log.push((dtx_id, DtxState::Aborted));
                if inner.completed_log.len() > inner.max_completed_log {
                    inner.completed_log.remove(0);
                }
                return Ok(Some(DtxState::Aborted));
            }
        }

        Ok(None)
    }

    /// Check for timed-out transactions and abort them.
    pub fn check_timeouts(&self) -> Vec<(DtxId, DtxMessage)> {
        let mut inner = self.inner.lock().unwrap();
        let mut aborts = Vec::new();

        let timed_out: Vec<DtxId> = inner
            .transactions
            .iter()
            .filter(|(_, dtx)| {
                dtx.phase_timed_out()
                    && !matches!(
                        dtx.state,
                        DtxState::Committed | DtxState::Aborted
                    )
            })
            .map(|(&id, _)| id)
            .collect();

        for dtx_id in timed_out {
            if let Some(dtx) = inner.transactions.get_mut(&dtx_id) {
                dtx.state = DtxState::Aborting;
                dtx.decision_log
                    .push(format!("DTX-{}: TIMEOUT in state {}", dtx_id, dtx.state));
                inner.stats.total_timeouts += 1;
                aborts.push((
                    dtx_id,
                    DtxMessage::Abort {
                        dtx_id,
                        reason: "coordinator timeout".to_string(),
                    },
                ));
            }
        }

        aborts
    }

    /// Get the current state of a distributed transaction.
    pub fn get_state(&self, dtx_id: DtxId) -> Option<DtxState> {
        let inner = self.inner.lock().unwrap();
        inner.transactions.get(&dtx_id).map(|dtx| dtx.state)
    }

    /// Get the decision log for a transaction.
    pub fn get_decision_log(&self, dtx_id: DtxId) -> Option<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        inner
            .transactions
            .get(&dtx_id)
            .map(|dtx| dtx.decision_log.clone())
    }

    /// Get coordinator statistics.
    pub fn stats(&self) -> DtxStats {
        let inner = self.inner.lock().unwrap();
        inner.stats.clone()
    }

    /// Remove a completed transaction from active tracking.
    pub fn cleanup(&self, dtx_id: DtxId) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.transactions.remove(&dtx_id).is_some()
    }
}

impl Default for DtxCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Participant-side handler for distributed transactions.
///
/// Each node runs a `DtxParticipant` that processes incoming 2PC messages
/// and manages local transaction state.
pub struct DtxParticipant {
    inner: Mutex<ParticipantInner>,
}

struct ParticipantInner {
    /// This participant's ID.
    node_id: ParticipantId,
    /// Prepared (but not yet committed) transactions: dtx_id → SQL.
    prepared: HashMap<DtxId, PreparedTransaction>,
    /// Statistics.
    stats: ParticipantStats,
}

/// A locally-prepared distributed transaction (awaiting commit/abort).
#[derive(Debug, Clone)]
pub struct PreparedTransaction {
    pub dtx_id: DtxId,
    pub sql: String,
    pub prepared_at: Instant,
    /// Whether the local execution succeeded (used for vote decision).
    pub local_ok: bool,
    /// Local error message if execution failed.
    pub local_error: Option<String>,
}

/// Participant statistics.
#[derive(Debug, Clone, Default)]
pub struct ParticipantStats {
    pub total_prepares: u64,
    pub total_commits: u64,
    pub total_aborts: u64,
    pub active_prepared: usize,
}

impl DtxParticipant {
    /// Create a new participant.
    pub fn new(node_id: ParticipantId) -> Self {
        Self {
            inner: Mutex::new(ParticipantInner {
                node_id,
                prepared: HashMap::new(),
                stats: ParticipantStats::default(),
            }),
        }
    }

    /// Handle a PREPARE message.
    ///
    /// Executes the SQL locally in a "prepare" mode (BEGIN + execute but don't commit).
    /// Returns a VoteResult message.
    ///
    /// `local_execute` is called with the SQL to attempt local execution.
    /// It should return `Ok(())` if the participant can commit, or `Err(reason)` if not.
    pub fn handle_prepare<F>(
        &self,
        dtx_id: DtxId,
        sql: String,
        local_execute: F,
    ) -> DtxMessage
    where
        F: FnOnce(&str) -> Result<(), String>,
    {
        let mut inner = self.inner.lock().unwrap();
        inner.stats.total_prepares += 1;

        let result = local_execute(&sql);
        let (vote, reason, local_ok, local_error) = match result {
            Ok(()) => (Vote::Commit, None, true, None),
            Err(e) => (Vote::Abort, Some(e.clone()), false, Some(e)),
        };

        let prepared = PreparedTransaction {
            dtx_id,
            sql,
            prepared_at: Instant::now(),
            local_ok,
            local_error,
        };
        inner.prepared.insert(dtx_id, prepared);
        inner.stats.active_prepared = inner.prepared.len();

        DtxMessage::VoteResult {
            dtx_id,
            participant: inner.node_id,
            vote,
            reason,
        }
    }

    /// Handle a COMMIT message — finalize the local transaction.
    ///
    /// `local_commit` is called to actually commit the prepared transaction.
    pub fn handle_commit<F>(&self, dtx_id: DtxId, local_commit: F) -> DtxMessage
    where
        F: FnOnce(DtxId) -> bool,
    {
        let mut inner = self.inner.lock().unwrap();
        let success = local_commit(dtx_id);
        inner.prepared.remove(&dtx_id);
        inner.stats.total_commits += 1;
        inner.stats.active_prepared = inner.prepared.len();

        DtxMessage::Ack {
            dtx_id,
            participant: inner.node_id,
            success,
        }
    }

    /// Handle an ABORT message — rollback the local transaction.
    ///
    /// `local_abort` is called to rollback the prepared transaction.
    pub fn handle_abort<F>(&self, dtx_id: DtxId, local_abort: F) -> DtxMessage
    where
        F: FnOnce(DtxId) -> bool,
    {
        let mut inner = self.inner.lock().unwrap();
        let success = local_abort(dtx_id);
        inner.prepared.remove(&dtx_id);
        inner.stats.total_aborts += 1;
        inner.stats.active_prepared = inner.prepared.len();

        DtxMessage::Ack {
            dtx_id,
            participant: inner.node_id,
            success,
        }
    }

    /// Handle a PRE_COMMIT message (3PC).
    pub fn handle_pre_commit(&self, dtx_id: DtxId) -> DtxMessage {
        let inner = self.inner.lock().unwrap();
        DtxMessage::PreCommitAck {
            dtx_id,
            participant: inner.node_id,
        }
    }

    /// Get participant statistics.
    pub fn stats(&self) -> ParticipantStats {
        let inner = self.inner.lock().unwrap();
        inner.stats.clone()
    }

    /// Get the node ID.
    pub fn node_id(&self) -> ParticipantId {
        let inner = self.inner.lock().unwrap();
        inner.node_id
    }

    /// Check if a transaction is currently prepared.
    pub fn is_prepared(&self, dtx_id: DtxId) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.prepared.contains_key(&dtx_id)
    }

    /// Get the number of currently prepared transactions.
    pub fn prepared_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.prepared.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_2pc_happy_path() {
        let coord = DtxCoordinator::new();
        let p1 = DtxParticipant::new(1);
        let p2 = DtxParticipant::new(2);

        // Begin distributed transaction
        let (dtx_id, prepare_msgs) = coord.begin(
            "INSERT INTO t VALUES (1)".to_string(),
            vec![1, 2],
            false, // 2PC
            Duration::from_secs(5),
        );
        assert_eq!(prepare_msgs.len(), 2);
        assert_eq!(coord.get_state(dtx_id), Some(DtxState::Init));

        // Participants handle PREPARE
        let vote1 = p1.handle_prepare(dtx_id, "INSERT INTO t VALUES (1)".to_string(), |_| Ok(()));
        let vote2 = p2.handle_prepare(dtx_id, "INSERT INTO t VALUES (1)".to_string(), |_| Ok(()));

        // Process votes
        match vote1 {
            DtxMessage::VoteResult { vote, participant, .. } => {
                assert_eq!(vote, Vote::Commit);
                let msgs = coord.process_vote(dtx_id, participant, vote, None).unwrap();
                assert!(msgs.is_empty()); // still waiting for p2
            }
            _ => panic!("expected VoteResult"),
        }
        match vote2 {
            DtxMessage::VoteResult { vote, participant, .. } => {
                assert_eq!(vote, Vote::Commit);
                let msgs = coord.process_vote(dtx_id, participant, vote, None).unwrap();
                // All voted commit — should get COMMIT message
                assert_eq!(msgs.len(), 1);
                assert!(matches!(msgs[0], DtxMessage::Commit { .. }));
            }
            _ => panic!("expected VoteResult"),
        }

        assert_eq!(coord.get_state(dtx_id), Some(DtxState::Committing));

        // Participants handle COMMIT
        let ack1 = p1.handle_commit(dtx_id, |_| true);
        let ack2 = p2.handle_commit(dtx_id, |_| true);

        match ack1 {
            DtxMessage::Ack { participant, success, .. } => {
                assert!(success);
                let result = coord.process_ack(dtx_id, participant, true).unwrap();
                assert_eq!(result, None); // still waiting for p2
            }
            _ => panic!("expected Ack"),
        }
        match ack2 {
            DtxMessage::Ack { participant, success, .. } => {
                assert!(success);
                let result = coord.process_ack(dtx_id, participant, true).unwrap();
                assert_eq!(result, Some(DtxState::Committed));
            }
            _ => panic!("expected Ack"),
        }

        let stats = coord.stats();
        assert_eq!(stats.total_committed, 1);
        assert_eq!(stats.total_aborted, 0);
    }

    #[test]
    fn test_2pc_abort_on_vote() {
        let coord = DtxCoordinator::new();
        let p1 = DtxParticipant::new(1);
        let p2 = DtxParticipant::new(2);

        let (dtx_id, _) = coord.begin(
            "INSERT INTO t VALUES (1)".to_string(),
            vec![1, 2],
            false,
            Duration::from_secs(5),
        );

        // p1 votes commit, p2 votes abort
        let _vote1 = p1.handle_prepare(dtx_id, "sql".to_string(), |_| Ok(()));
        let vote2 = p2.handle_prepare(dtx_id, "sql".to_string(), |_| {
            Err("constraint violation".to_string())
        });

        coord.process_vote(dtx_id, 1, Vote::Commit, None).unwrap();

        match vote2 {
            DtxMessage::VoteResult { vote, participant, reason, .. } => {
                assert_eq!(vote, Vote::Abort);
                let msgs = coord.process_vote(dtx_id, participant, vote, reason).unwrap();
                assert_eq!(msgs.len(), 1);
                assert!(matches!(msgs[0], DtxMessage::Abort { .. }));
            }
            _ => panic!("expected VoteResult"),
        }

        assert_eq!(coord.get_state(dtx_id), Some(DtxState::Aborting));

        // Process abort acks
        let ack1 = p1.handle_abort(dtx_id, |_| true);
        let ack2 = p2.handle_abort(dtx_id, |_| true);

        if let DtxMessage::Ack { participant, .. } = ack1 {
            coord.process_ack(dtx_id, participant, false).unwrap();
        }
        if let DtxMessage::Ack { participant, .. } = ack2 {
            let result = coord.process_ack(dtx_id, participant, false).unwrap();
            assert_eq!(result, Some(DtxState::Aborted));
        }

        let stats = coord.stats();
        assert_eq!(stats.total_aborted, 1);
    }

    #[test]
    fn test_3pc_happy_path() {
        let coord = DtxCoordinator::new();
        let p1 = DtxParticipant::new(1);
        let p2 = DtxParticipant::new(2);

        let (dtx_id, _) = coord.begin(
            "UPDATE t SET v=1".to_string(),
            vec![1, 2],
            true, // 3PC
            Duration::from_secs(5),
        );

        // Both vote commit
        p1.handle_prepare(dtx_id, "sql".to_string(), |_| Ok(()));
        p2.handle_prepare(dtx_id, "sql".to_string(), |_| Ok(()));

        coord.process_vote(dtx_id, 1, Vote::Commit, None).unwrap();
        let msgs = coord.process_vote(dtx_id, 2, Vote::Commit, None).unwrap();
        // Should get PreCommit message (3PC)
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], DtxMessage::PreCommit { .. }));
        assert_eq!(coord.get_state(dtx_id), Some(DtxState::PreCommitted));

        // Pre-commit acks
        let pc_ack1 = p1.handle_pre_commit(dtx_id);
        let pc_ack2 = p2.handle_pre_commit(dtx_id);

        if let DtxMessage::PreCommitAck { participant, .. } = pc_ack1 {
            let msgs = coord.process_pre_commit_ack(dtx_id, participant).unwrap();
            assert!(msgs.is_empty());
        }
        if let DtxMessage::PreCommitAck { participant, .. } = pc_ack2 {
            let msgs = coord.process_pre_commit_ack(dtx_id, participant).unwrap();
            assert_eq!(msgs.len(), 1);
            assert!(matches!(msgs[0], DtxMessage::Commit { .. }));
        }

        assert_eq!(coord.get_state(dtx_id), Some(DtxState::Committing));

        // Final commit acks
        let ack1 = p1.handle_commit(dtx_id, |_| true);
        let ack2 = p2.handle_commit(dtx_id, |_| true);
        if let DtxMessage::Ack { participant, .. } = ack1 {
            coord.process_ack(dtx_id, participant, true).unwrap();
        }
        if let DtxMessage::Ack { participant, .. } = ack2 {
            let result = coord.process_ack(dtx_id, participant, true).unwrap();
            assert_eq!(result, Some(DtxState::Committed));
        }
    }

    #[test]
    fn test_coordinator_timeout() {
        let coord = DtxCoordinator::new();

        let (dtx_id, _) = coord.begin(
            "sql".to_string(),
            vec![1, 2],
            false,
            Duration::from_millis(1), // immediate timeout
        );

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(5));

        let timeouts = coord.check_timeouts();
        assert!(!timeouts.is_empty());
        assert_eq!(timeouts[0].0, dtx_id);
        assert!(matches!(timeouts[0].1, DtxMessage::Abort { .. }));

        let stats = coord.stats();
        assert_eq!(stats.total_timeouts, 1);
    }

    #[test]
    fn test_participant_state_tracking() {
        let p = DtxParticipant::new(42);
        assert_eq!(p.node_id(), 42);
        assert_eq!(p.prepared_count(), 0);

        p.handle_prepare(1, "sql".to_string(), |_| Ok(()));
        assert!(p.is_prepared(1));
        assert_eq!(p.prepared_count(), 1);

        p.handle_commit(1, |_| true);
        assert!(!p.is_prepared(1));
        assert_eq!(p.prepared_count(), 0);

        let stats = p.stats();
        assert_eq!(stats.total_prepares, 1);
        assert_eq!(stats.total_commits, 1);
    }

    #[test]
    fn test_coordinator_cleanup() {
        let coord = DtxCoordinator::new();
        let (dtx_id, _) = coord.begin("sql".to_string(), vec![1], false, Duration::from_secs(5));
        assert!(coord.get_state(dtx_id).is_some());
        assert!(coord.cleanup(dtx_id));
        assert!(coord.get_state(dtx_id).is_none());
        assert!(!coord.cleanup(999));
    }

    #[test]
    fn test_decision_log() {
        let coord = DtxCoordinator::new();
        let (dtx_id, _) = coord.begin("sql".to_string(), vec![1], false, Duration::from_secs(5));

        let log = coord.get_decision_log(dtx_id).unwrap();
        assert!(!log.is_empty());
        assert!(log[0].contains("created"));

        coord.process_vote(dtx_id, 1, Vote::Commit, None).unwrap();
        let log = coord.get_decision_log(dtx_id).unwrap();
        assert!(log.len() >= 2);
    }

    #[test]
    fn test_dtx_state_display() {
        assert_eq!(format!("{}", DtxState::Init), "INIT");
        assert_eq!(format!("{}", DtxState::Preparing), "PREPARING");
        assert_eq!(format!("{}", DtxState::PreCommitted), "PRE_COMMITTED");
        assert_eq!(format!("{}", DtxState::Committing), "COMMITTING");
        assert_eq!(format!("{}", DtxState::Aborting), "ABORTING");
        assert_eq!(format!("{}", DtxState::Committed), "COMMITTED");
        assert_eq!(format!("{}", DtxState::Aborted), "ABORTED");
    }

    #[test]
    fn test_duplicate_vote_error() {
        let coord = DtxCoordinator::new();
        let (dtx_id, _) = coord.begin("sql".to_string(), vec![1, 2], false, Duration::from_secs(5));

        coord.process_vote(dtx_id, 1, Vote::Commit, None).unwrap();
        let result = coord.process_vote(dtx_id, 1, Vote::Commit, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_unknown_participant_error() {
        let coord = DtxCoordinator::new();
        let (dtx_id, _) = coord.begin("sql".to_string(), vec![1], false, Duration::from_secs(5));

        let result = coord.process_vote(dtx_id, 999, Vote::Commit, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_participant_abort_handler() {
        let p = DtxParticipant::new(1);
        p.handle_prepare(1, "sql".to_string(), |_| Ok(()));
        let ack = p.handle_abort(1, |_| true);
        assert!(matches!(ack, DtxMessage::Ack { success: true, .. }));
        assert!(!p.is_prepared(1));
        let stats = p.stats();
        assert_eq!(stats.total_aborts, 1);
    }

    #[test]
    fn test_coordinator_default() {
        let coord = DtxCoordinator::default();
        let stats = coord.stats();
        assert_eq!(stats.total_transactions, 0);
    }
}
