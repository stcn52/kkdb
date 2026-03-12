//! Round-based coverage tests (R8-R18) — each round's feature coverage

pub(crate) use super::{exec, exec_multi, query_rows};
pub(crate) use crate::vm::execute::{VM, ExecResult, like_match};
pub(crate) use crate::types::Value;

mod coverage_r8_optimizer_wal;
mod coverage_r9_mvcc_fts_raft;
mod coverage_r10_prepared_bloom_wf;
mod coverage_r11_audit_hash_histogram;
mod coverage_r12_join_rbac_lsm_perf;
mod coverage_r13_vector_ha_gc_diag;
mod coverage_r14_bufpool_compiler_dtx_obs;
mod coverage_r15_storage_exec_cluster_devtools;
mod coverage_r16_optimizer_sqleng_dtx_security;
mod coverage_r17_ultimate_optdeep_hadr_obsops;
mod coverage_r18_adv_query;
mod coverage_r20_storage_disttxn_sqlext_secadv;
mod coverage_r21_exec2_fts_vec_obs;
