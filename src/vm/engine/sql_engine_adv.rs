// R16 – SQL execution engine ultimate: materialized view incremental refresh,
//       cursor paging, async pipeline, expression JIT compilation,
//       execution plan cache eviction.
//
// Provides:
//   - `MaterializedView`: view definition with incremental refresh tracking
//   - `CursorPager`: keyed + offset cursor-based pagination
//   - `AsyncPipeline`: multi-stage pipeline with ready/blocked semantics
//   - `ExprJitCompiler`: bytecode → optimized instruction sequence
//   - `PlanCacheEvictor`: LFU-based execution plan cache eviction

use std::collections::HashMap;

// ── Materialized View ─────────────────────────────────────────────────

/// Change type for incremental refresh.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeType {
    Insert,
    Update,
    Delete,
}

/// A pending change to a materialized view's source.
#[derive(Debug, Clone)]
pub struct ViewChange {
    pub change_type: ChangeType,
    pub table: String,
    pub row_id: u64,
    pub timestamp: u64,
}

/// Materialized view with incremental refresh support.
pub struct MaterializedView {
    pub name: String,
    pub query: String,
    pub source_tables: Vec<String>,
    last_refresh: u64,
    pending_changes: Vec<ViewChange>,
    row_count: usize,
    is_stale: bool,
}

impl MaterializedView {
    pub fn new(name: &str, query: &str, source_tables: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            query: query.to_string(),
            source_tables,
            last_refresh: 0,
            pending_changes: Vec::new(),
            row_count: 0,
            is_stale: true,
        }
    }

    /// Record a change to a source table.
    pub fn on_source_change(&mut self, change: ViewChange) {
        if self.source_tables.contains(&change.table) {
            self.pending_changes.push(change);
            self.is_stale = true;
        }
    }

    /// Perform a full refresh.
    pub fn full_refresh(&mut self, new_row_count: usize, timestamp: u64) {
        self.pending_changes.clear();
        self.row_count = new_row_count;
        self.last_refresh = timestamp;
        self.is_stale = false;
    }

    /// Perform an incremental refresh (apply pending changes).
    pub fn incremental_refresh(&mut self, timestamp: u64) -> usize {
        let count = self.pending_changes.len();
        for change in &self.pending_changes {
            match change.change_type {
                ChangeType::Insert => self.row_count += 1,
                ChangeType::Delete => self.row_count = self.row_count.saturating_sub(1),
                ChangeType::Update => {} // row count unchanged
            }
        }
        self.pending_changes.clear();
        self.last_refresh = timestamp;
        self.is_stale = false;
        count
    }

    pub fn is_stale(&self) -> bool {
        self.is_stale
    }

    pub fn pending_count(&self) -> usize {
        self.pending_changes.len()
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn last_refresh(&self) -> u64 {
        self.last_refresh
    }
}

// ── Cursor Pager ──────────────────────────────────────────────────────

/// Cursor state for paginated queries.
#[derive(Debug, Clone)]
pub struct CursorState {
    pub cursor_id: u64,
    pub query: String,
    pub offset: usize,
    pub page_size: usize,
    pub total_rows: Option<usize>,
    pub last_key: Option<i64>,
    pub is_exhausted: bool,
}

/// Manages cursor-based pagination.
pub struct CursorPager {
    cursors: HashMap<u64, CursorState>,
    next_id: u64,
}

impl CursorPager {
    pub fn new() -> Self {
        Self {
            cursors: HashMap::new(),
            next_id: 1,
        }
    }

    /// Open a new cursor.
    pub fn open(&mut self, query: &str, page_size: usize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.cursors.insert(
            id,
            CursorState {
                cursor_id: id,
                query: query.to_string(),
                offset: 0,
                page_size,
                total_rows: None,
                last_key: None,
                is_exhausted: false,
            },
        );
        id
    }

    /// Fetch next page: returns (offset, limit) for the next batch.
    pub fn next_page(&mut self, cursor_id: u64) -> Option<(usize, usize)> {
        if let Some(cursor) = self.cursors.get_mut(&cursor_id) {
            if cursor.is_exhausted {
                return None;
            }
            let offset = cursor.offset;
            let limit = cursor.page_size;
            cursor.offset += limit;
            // Check if exhausted
            if let Some(total) = cursor.total_rows {
                if cursor.offset >= total {
                    cursor.is_exhausted = true;
                }
            }
            Some((offset, limit))
        } else {
            None
        }
    }

    /// Set total row count (enables exhaustion detection).
    pub fn set_total(&mut self, cursor_id: u64, total: usize) {
        if let Some(cursor) = self.cursors.get_mut(&cursor_id) {
            cursor.total_rows = Some(total);
            if cursor.offset >= total {
                cursor.is_exhausted = true;
            }
        }
    }

    /// Set keyset cursor position.
    pub fn set_last_key(&mut self, cursor_id: u64, key: i64) {
        if let Some(cursor) = self.cursors.get_mut(&cursor_id) {
            cursor.last_key = Some(key);
        }
    }

    /// Close a cursor.
    pub fn close(&mut self, cursor_id: u64) -> bool {
        self.cursors.remove(&cursor_id).is_some()
    }

    pub fn active_cursors(&self) -> usize {
        self.cursors.len()
    }

    pub fn is_exhausted(&self, cursor_id: u64) -> bool {
        self.cursors
            .get(&cursor_id)
            .map(|c| c.is_exhausted)
            .unwrap_or(true)
    }
}

// ── Async Pipeline ────────────────────────────────────────────────────

/// Pipeline stage state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StageState {
    Ready,
    Running,
    Blocked,
    Completed,
    Failed,
}

/// A stage in the async execution pipeline.
#[derive(Debug, Clone)]
pub struct PipelineStage {
    pub name: String,
    pub state: StageState,
    pub rows_produced: usize,
    pub rows_consumed: usize,
}

impl PipelineStage {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            state: StageState::Ready,
            rows_produced: 0,
            rows_consumed: 0,
        }
    }

    pub fn produce(&mut self, count: usize) {
        self.rows_produced += count;
        self.state = StageState::Running;
    }

    pub fn consume(&mut self, count: usize) {
        self.rows_consumed += count;
    }

    pub fn complete(&mut self) {
        self.state = StageState::Completed;
    }

    pub fn block(&mut self) {
        self.state = StageState::Blocked;
    }

    pub fn unblock(&mut self) {
        if self.state == StageState::Blocked {
            self.state = StageState::Ready;
        }
    }

    pub fn buffer_size(&self) -> usize {
        self.rows_produced.saturating_sub(self.rows_consumed)
    }
}

/// Multi-stage async execution pipeline.
pub struct AsyncPipeline {
    stages: Vec<PipelineStage>,
}

impl AsyncPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn add_stage(&mut self, name: &str) -> usize {
        let idx = self.stages.len();
        self.stages.push(PipelineStage::new(name));
        idx
    }

    pub fn stage(&self, idx: usize) -> Option<&PipelineStage> {
        self.stages.get(idx)
    }

    pub fn stage_mut(&mut self, idx: usize) -> Option<&mut PipelineStage> {
        self.stages.get_mut(idx)
    }

    pub fn is_complete(&self) -> bool {
        self.stages.iter().all(|s| s.state == StageState::Completed)
    }

    pub fn has_blocked(&self) -> bool {
        self.stages.iter().any(|s| s.state == StageState::Blocked)
    }

    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }

    pub fn total_produced(&self) -> usize {
        self.stages.iter().map(|s| s.rows_produced).sum()
    }
}

// ── Expression JIT Compiler ───────────────────────────────────────────

/// JIT instruction (simplified IR).
#[derive(Debug, Clone, PartialEq)]
pub enum JitOp {
    LoadImm(i64),
    LoadReg(usize),
    StoreReg(usize),
    Add,
    Sub,
    Mul,
    Div,
    Cmp,       // push 1 if top-1 == top, else 0
    Jz(usize), // jump if zero to instruction index
    Jmp(usize),
    Ret,
}

/// A JIT-compiled expression.
pub struct JitCompiledExpr {
    instructions: Vec<JitOp>,
    register_count: usize,
}

impl JitCompiledExpr {
    pub fn new(instructions: Vec<JitOp>, register_count: usize) -> Self {
        Self {
            instructions,
            register_count,
        }
    }

    /// Evaluate the expression with the given register values.
    pub fn eval(&self, inputs: &[i64]) -> i64 {
        let mut regs = vec![0i64; self.register_count];
        for (i, &v) in inputs.iter().enumerate() {
            if i < regs.len() {
                regs[i] = v;
            }
        }
        let mut stack: Vec<i64> = Vec::new();
        let mut pc = 0usize;

        while pc < self.instructions.len() {
            match &self.instructions[pc] {
                JitOp::LoadImm(v) => stack.push(*v),
                JitOp::LoadReg(r) => stack.push(regs.get(*r).copied().unwrap_or(0)),
                JitOp::StoreReg(r) => {
                    if let Some(v) = stack.pop() {
                        if *r < regs.len() {
                            regs[*r] = v;
                        }
                    }
                }
                JitOp::Add => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(a.wrapping_add(b));
                }
                JitOp::Sub => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(a.wrapping_sub(b));
                }
                JitOp::Mul => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(a.wrapping_mul(b));
                }
                JitOp::Div => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if b != 0 { a / b } else { 0 });
                }
                JitOp::Cmp => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a == b { 1 } else { 0 });
                }
                JitOp::Jz(target) => {
                    let v = stack.pop().unwrap_or(0);
                    if v == 0 {
                        pc = *target;
                        continue;
                    }
                }
                JitOp::Jmp(target) => {
                    pc = *target;
                    continue;
                }
                JitOp::Ret => {
                    return stack.pop().unwrap_or(0);
                }
            }
            pc += 1;
        }
        stack.pop().unwrap_or(0)
    }

    pub fn instruction_count(&self) -> usize {
        self.instructions.len()
    }
}

// ── Plan Cache Evictor ────────────────────────────────────────────────

/// Cached execution plan entry.
#[derive(Debug, Clone)]
pub struct CachedPlan {
    pub plan_id: u64,
    pub sql_hash: u64,
    pub use_count: u64,
    pub last_used: u64,
    pub cost: f64,
    pub byte_size: usize,
}

/// LFU-based plan cache eviction.
pub struct PlanCacheEvictor {
    plans: HashMap<u64, CachedPlan>,
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    evictions: u64,
}

impl PlanCacheEvictor {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            plans: HashMap::new(),
            max_entries,
            max_bytes,
            total_bytes: 0,
            evictions: 0,
        }
    }

    /// Insert a plan, evicting if necessary.
    pub fn insert(&mut self, plan: CachedPlan) {
        while self.plans.len() >= self.max_entries
            || self.total_bytes + plan.byte_size > self.max_bytes
        {
            if !self.evict_one() {
                break;
            }
        }
        self.total_bytes += plan.byte_size;
        self.plans.insert(plan.plan_id, plan);
    }

    /// Use a plan (increment use count).
    pub fn touch(&mut self, plan_id: u64, timestamp: u64) -> bool {
        if let Some(plan) = self.plans.get_mut(&plan_id) {
            plan.use_count += 1;
            plan.last_used = timestamp;
            true
        } else {
            false
        }
    }

    /// Evict the least frequently used plan.
    fn evict_one(&mut self) -> bool {
        let victim = self
            .plans
            .values()
            .min_by_key(|p| p.use_count)
            .map(|p| p.plan_id);
        if let Some(id) = victim {
            if let Some(plan) = self.plans.remove(&id) {
                self.total_bytes = self.total_bytes.saturating_sub(plan.byte_size);
                self.evictions += 1;
                return true;
            }
        }
        false
    }

    pub fn get(&self, plan_id: u64) -> Option<&CachedPlan> {
        self.plans.get(&plan_id)
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn eviction_count(&self) -> u64 {
        self.evictions
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialized_view_incremental() {
        let mut mv = MaterializedView::new(
            "active_users",
            "SELECT * FROM users WHERE active = 1",
            vec!["users".to_string()],
        );
        mv.full_refresh(100, 1);
        assert_eq!(mv.row_count(), 100);
        assert!(!mv.is_stale());

        mv.on_source_change(ViewChange {
            change_type: ChangeType::Insert,
            table: "users".to_string(),
            row_id: 1,
            timestamp: 2,
        });
        assert!(mv.is_stale());
        let applied = mv.incremental_refresh(2);
        assert_eq!(applied, 1);
        assert_eq!(mv.row_count(), 101);
    }

    #[test]
    fn cursor_pager_pagination() {
        let mut pager = CursorPager::new();
        let id = pager.open("SELECT * FROM t", 10);
        pager.set_total(id, 25);
        assert_eq!(pager.next_page(id), Some((0, 10)));
        assert_eq!(pager.next_page(id), Some((10, 10)));
        assert_eq!(pager.next_page(id), Some((20, 10)));
        assert!(pager.is_exhausted(id));
        assert_eq!(pager.next_page(id), None);
        assert!(pager.close(id));
        assert_eq!(pager.active_cursors(), 0);
    }

    #[test]
    fn async_pipeline_stages() {
        let mut pipe = AsyncPipeline::new();
        let s1 = pipe.add_stage("scan");
        let s2 = pipe.add_stage("filter");
        pipe.stage_mut(s1).unwrap().produce(100);
        pipe.stage_mut(s2).unwrap().consume(50);
        assert!(!pipe.is_complete());
        pipe.stage_mut(s1).unwrap().complete();
        pipe.stage_mut(s2).unwrap().complete();
        assert!(pipe.is_complete());
        assert_eq!(pipe.stage_count(), 2);
    }

    #[test]
    fn async_pipeline_blocked() {
        let mut pipe = AsyncPipeline::new();
        let s = pipe.add_stage("io");
        pipe.stage_mut(s).unwrap().block();
        assert!(pipe.has_blocked());
        pipe.stage_mut(s).unwrap().unblock();
        assert!(!pipe.has_blocked());
    }

    #[test]
    fn jit_compiled_expr_arithmetic() {
        // Compute: reg[0] + reg[1] * 2
        let expr = JitCompiledExpr::new(
            vec![
                JitOp::LoadReg(1),
                JitOp::LoadImm(2),
                JitOp::Mul,
                JitOp::LoadReg(0),
                JitOp::Add,
                JitOp::Ret,
            ],
            2,
        );
        assert_eq!(expr.eval(&[10, 5]), 20); // 10 + 5*2 = 20
        assert_eq!(expr.instruction_count(), 6);
    }

    #[test]
    fn jit_compiled_expr_cmp_and_jz() {
        // If reg[0] == 42, return 1; else return 0
        let expr = JitCompiledExpr::new(
            vec![
                JitOp::LoadReg(0),  // 0
                JitOp::LoadImm(42), // 1
                JitOp::Cmp,         // 2
                JitOp::Jz(5),       // 3: if not equal, jump to 5
                JitOp::LoadImm(1),  // 4
                JitOp::Ret,         // 5
            ],
            1,
        );
        assert_eq!(expr.eval(&[42]), 1);
        assert_eq!(expr.eval(&[99]), 0); // Jz jumps past LoadImm(1), stack empty → 0
    }

    #[test]
    fn plan_cache_evictor_lfu() {
        let mut cache = PlanCacheEvictor::new(3, 10000);
        for i in 1..=3 {
            cache.insert(CachedPlan {
                plan_id: i,
                sql_hash: i * 100,
                use_count: 0,
                last_used: 0,
                cost: 10.0,
                byte_size: 100,
            });
        }
        // Plan 2 gets used a lot
        for _ in 0..10 {
            cache.touch(2, 1);
        }
        // Insert a 4th → should evict plan 1 or 3 (least used)
        cache.insert(CachedPlan {
            plan_id: 4,
            sql_hash: 400,
            use_count: 0,
            last_used: 0,
            cost: 5.0,
            byte_size: 100,
        });
        assert_eq!(cache.len(), 3);
        assert!(cache.get(2).is_some()); // plan 2 survived
        assert!(cache.eviction_count() > 0);
    }

    #[test]
    fn plan_cache_byte_limit() {
        let mut cache = PlanCacheEvictor::new(100, 500);
        for i in 1..=5 {
            cache.insert(CachedPlan {
                plan_id: i,
                sql_hash: i,
                use_count: 0,
                last_used: 0,
                cost: 1.0,
                byte_size: 200,
            });
        }
        assert!(cache.total_bytes() <= 500);
    }
}
