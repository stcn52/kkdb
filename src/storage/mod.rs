// ── Core storage (stay at root) ────────────────────────────────────────
pub mod backup;
pub mod bloom;
pub mod buffer_pool;
pub mod btree;
pub mod cursor;
pub mod lsm;
pub mod pager;
pub mod prefix_compress;
pub mod wal;

// ── Extended / optimization modules ───────────────────────────────────
pub mod ext;          // advanced, optimizer, ultimate

// ── Backward-compatible re-exports ────────────────────────────────────
pub use ext::advanced;
pub use ext::optimizer;
pub use ext::ultimate;
pub use ext::adv_storage;
pub use ext::deep_storage;
