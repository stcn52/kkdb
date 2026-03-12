// R14 – Query compilation & code generation: query template compilation,
//       expression vectorized codegen, runtime specialization, adaptive
//       recompilation.
//
// Provides:
//   - `QueryTemplate`: parameterized query template with slot substitution
//   - `ExprCodegen`: expression → vectorized code plan
//   - `RuntimeSpecializer`: specialize a plan for known constant parameters
//   - `RecompilationTracker`: tracks execution stats to trigger recompilation

use std::collections::HashMap;
use std::time::{Duration, Instant};

// ── Query Template ────────────────────────────────────────────────────

/// A compiled query template with parameter slots.
#[derive(Debug, Clone)]
pub struct QueryTemplate {
    pub name: String,
    pub sql_template: String,
    /// Parameter slot names (e.g. "$1", "$2").
    pub param_slots: Vec<String>,
    /// Cached plan hash for invalidation detection.
    pub plan_hash: u64,
    pub compile_time: Duration,
    pub use_count: u64,
}

impl QueryTemplate {
    pub fn new(name: &str, sql: &str, params: Vec<String>, compile_time: Duration) -> Self {
        let plan_hash = Self::compute_hash(sql);
        Self {
            name: name.to_string(),
            sql_template: sql.to_string(),
            param_slots: params,
            plan_hash,
            compile_time,
            use_count: 0,
        }
    }

    fn compute_hash(sql: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset
        for b in sql.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Substitute parameters into the template.
    pub fn bind(&mut self, params: &[&str]) -> String {
        self.use_count += 1;
        let mut sql = self.sql_template.clone();
        for (i, slot) in self.param_slots.iter().enumerate() {
            if let Some(val) = params.get(i) {
                sql = sql.replace(slot, val);
            }
        }
        sql
    }

    pub fn param_count(&self) -> usize {
        self.param_slots.len()
    }

    /// Check if the template needs recompilation (plan hash changed).
    pub fn needs_recompile(&self, new_sql: &str) -> bool {
        Self::compute_hash(new_sql) != self.plan_hash
    }
}

/// Cache of compiled query templates.
pub struct TemplateCache {
    templates: HashMap<String, QueryTemplate>,
    max_size: usize,
}

impl TemplateCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            templates: HashMap::new(),
            max_size,
        }
    }

    pub fn insert(&mut self, template: QueryTemplate) -> bool {
        if self.templates.len() >= self.max_size && !self.templates.contains_key(&template.name) {
            // Evict least used
            if let Some(victim) = self.templates.values()
                .min_by_key(|t| t.use_count)
                .map(|t| t.name.clone())
            {
                self.templates.remove(&victim);
            }
        }
        self.templates.insert(template.name.clone(), template);
        true
    }

    pub fn get(&self, name: &str) -> Option<&QueryTemplate> {
        self.templates.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut QueryTemplate> {
        self.templates.get_mut(name)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.templates.remove(name).is_some()
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }
}

// ── Expression Codegen ────────────────────────────────────────────────

/// Instruction in a vectorized expression evaluation plan.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeOp {
    LoadConst(i64),
    LoadCol(usize),
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Lt,
    Gt,
    And,
    Or,
    Not,
    /// Store result to output register.
    Store(usize),
}

/// Compiled expression: a sequence of stack-based instructions.
#[derive(Debug, Clone)]
pub struct CompiledExpr {
    pub ops: Vec<CodeOp>,
    pub output_register: usize,
}

impl CompiledExpr {
    pub fn new(ops: Vec<CodeOp>, output_register: usize) -> Self {
        Self { ops, output_register }
    }

    /// Evaluate the compiled expression for a single row.
    pub fn eval(&self, row: &[i64]) -> i64 {
        let mut stack: Vec<i64> = Vec::new();
        for op in &self.ops {
            match op {
                CodeOp::LoadConst(v) => stack.push(*v),
                CodeOp::LoadCol(idx) => stack.push(row.get(*idx).copied().unwrap_or(0)),
                CodeOp::Add => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(a + b);
                }
                CodeOp::Sub => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(a - b);
                }
                CodeOp::Mul => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(a * b);
                }
                CodeOp::Div => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if b != 0 { a / b } else { 0 });
                }
                CodeOp::Eq => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a == b { 1 } else { 0 });
                }
                CodeOp::Lt => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a < b { 1 } else { 0 });
                }
                CodeOp::Gt => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a > b { 1 } else { 0 });
                }
                CodeOp::And => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a != 0 && b != 0 { 1 } else { 0 });
                }
                CodeOp::Or => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a != 0 || b != 0 { 1 } else { 0 });
                }
                CodeOp::Not => {
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if a == 0 { 1 } else { 0 });
                }
                CodeOp::Store(_) => { /* final result stays on stack */ }
            }
        }
        stack.pop().unwrap_or(0)
    }

    /// Evaluate for a batch of rows.
    pub fn eval_batch(&self, rows: &[Vec<i64>]) -> Vec<i64> {
        rows.iter().map(|r| self.eval(r)).collect()
    }

    pub fn instruction_count(&self) -> usize {
        self.ops.len()
    }
}

// ── Runtime Specializer ───────────────────────────────────────────────

/// Specializes a compiled expression for known constant parameters.
pub struct RuntimeSpecializer;

impl RuntimeSpecializer {
    /// Replace LoadCol instructions with LoadConst where the column value
    /// is known to be constant.
    pub fn specialize(expr: &CompiledExpr, constants: &HashMap<usize, i64>) -> CompiledExpr {
        let ops: Vec<CodeOp> = expr.ops.iter().map(|op| {
            match op {
                CodeOp::LoadCol(idx) if constants.contains_key(idx) => {
                    CodeOp::LoadConst(constants[idx])
                }
                other => other.clone(),
            }
        }).collect();
        CompiledExpr::new(ops, expr.output_register)
    }

    /// Peephole optimization: constant-fold consecutive LoadConst + op.
    pub fn peephole_fold(expr: &CompiledExpr) -> CompiledExpr {
        let mut ops = expr.ops.clone();
        let mut changed = true;
        while changed {
            changed = false;
            let mut i = 0;
            while i + 2 < ops.len() {
                if let (CodeOp::LoadConst(a), CodeOp::LoadConst(b)) = (&ops[i], &ops[i + 1]) {
                    let a = *a;
                    let b = *b;
                    let result = match &ops[i + 2] {
                        CodeOp::Add => Some(a + b),
                        CodeOp::Sub => Some(a - b),
                        CodeOp::Mul => Some(a * b),
                        CodeOp::Div => {
                            if b != 0 {
                                Some(a / b)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(r) = result {
                        ops[i] = CodeOp::LoadConst(r);
                        ops.remove(i + 2);
                        ops.remove(i + 1);
                        changed = true;
                        continue;
                    }
                }
                i += 1;
            }
        }
        CompiledExpr::new(ops, expr.output_register)
    }
}

// ── Recompilation Tracker ─────────────────────────────────────────────

/// Tracks execution stats to decide when queries should be recompiled.
pub struct RecompilationTracker {
    /// template_name → (execution_count, total_time, last_recompile)
    stats: HashMap<String, (u64, Duration, Instant)>,
    /// Threshold: recompile if avg time exceeds this.
    recompile_threshold: Duration,
    /// Minimum executions before considering recompilation.
    min_executions: u64,
}

impl RecompilationTracker {
    pub fn new(recompile_threshold: Duration, min_executions: u64) -> Self {
        Self {
            stats: HashMap::new(),
            recompile_threshold,
            min_executions,
        }
    }

    /// Record an execution.
    pub fn record(&mut self, name: &str, duration: Duration) {
        let entry = self.stats.entry(name.to_string())
            .or_insert_with(|| (0, Duration::ZERO, Instant::now()));
        entry.0 += 1;
        entry.1 += duration;
    }

    /// Check if a template should be recompiled.
    pub fn should_recompile(&self, name: &str) -> bool {
        if let Some(&(count, total, _)) = self.stats.get(name) {
            if count >= self.min_executions {
                let avg = total / count as u32;
                return avg > self.recompile_threshold;
            }
        }
        false
    }

    /// Mark that recompilation was performed.
    pub fn mark_recompiled(&mut self, name: &str) {
        if let Some(entry) = self.stats.get_mut(name) {
            entry.0 = 0;
            entry.1 = Duration::ZERO;
            entry.2 = Instant::now();
        }
    }

    /// Average execution time for a template.
    pub fn avg_time(&self, name: &str) -> Option<Duration> {
        self.stats.get(name).and_then(|&(count, total, _)| {
            if count > 0 {
                Some(total / count as u32)
            } else {
                None
            }
        })
    }

    pub fn execution_count(&self, name: &str) -> u64 {
        self.stats.get(name).map(|s| s.0).unwrap_or(0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_template_bind() {
        let mut t = QueryTemplate::new(
            "q1",
            "SELECT * FROM users WHERE id = $1 AND name = $2",
            vec!["$1".into(), "$2".into()],
            Duration::from_micros(100),
        );
        let sql = t.bind(&["42", "'alice'"]);
        assert_eq!(sql, "SELECT * FROM users WHERE id = 42 AND name = 'alice'");
        assert_eq!(t.use_count, 1);
    }

    #[test]
    fn query_template_needs_recompile() {
        let t = QueryTemplate::new("q1", "SELECT 1", vec![], Duration::ZERO);
        assert!(!t.needs_recompile("SELECT 1"));
        assert!(t.needs_recompile("SELECT 2"));
    }

    #[test]
    fn template_cache_insert_and_evict() {
        let mut cache = TemplateCache::new(2);
        cache.insert(QueryTemplate::new("a", "SELECT a", vec![], Duration::ZERO));
        cache.insert(QueryTemplate::new("b", "SELECT b", vec![], Duration::ZERO));
        // Make "a" more used
        cache.get_mut("a").unwrap().use_count = 10;
        // Insert "c" → should evict "b" (least used)
        cache.insert(QueryTemplate::new("c", "SELECT c", vec![], Duration::ZERO));
        assert_eq!(cache.len(), 2);
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn compiled_expr_eval() {
        // Expression: col[0] + col[1] * 2
        let expr = CompiledExpr::new(vec![
            CodeOp::LoadCol(0),
            CodeOp::LoadCol(1),
            CodeOp::LoadConst(2),
            CodeOp::Mul,
            CodeOp::Add,
            CodeOp::Store(0),
        ], 0);
        assert_eq!(expr.eval(&[10, 5]), 20); // 10 + 5*2 = 20
    }

    #[test]
    fn compiled_expr_comparison() {
        // col[0] > 5
        let expr = CompiledExpr::new(vec![
            CodeOp::LoadCol(0),
            CodeOp::LoadConst(5),
            CodeOp::Gt,
        ], 0);
        assert_eq!(expr.eval(&[10]), 1);
        assert_eq!(expr.eval(&[3]), 0);
    }

    #[test]
    fn compiled_expr_logic() {
        // col[0] > 0 AND col[1] > 0
        let expr = CompiledExpr::new(vec![
            CodeOp::LoadCol(0),
            CodeOp::LoadConst(0),
            CodeOp::Gt,
            CodeOp::LoadCol(1),
            CodeOp::LoadConst(0),
            CodeOp::Gt,
            CodeOp::And,
        ], 0);
        assert_eq!(expr.eval(&[1, 1]), 1);
        assert_eq!(expr.eval(&[0, 1]), 0);
    }

    #[test]
    fn compiled_expr_batch() {
        let expr = CompiledExpr::new(vec![
            CodeOp::LoadCol(0),
            CodeOp::LoadConst(1),
            CodeOp::Add,
        ], 0);
        let results = expr.eval_batch(&[vec![10], vec![20], vec![30]]);
        assert_eq!(results, vec![11, 21, 31]);
    }

    #[test]
    fn runtime_specializer_constant_sub() {
        let expr = CompiledExpr::new(vec![
            CodeOp::LoadCol(0),
            CodeOp::LoadCol(1),
            CodeOp::Add,
        ], 0);
        let mut consts = HashMap::new();
        consts.insert(1, 42);
        let specialized = RuntimeSpecializer::specialize(&expr, &consts);
        assert_eq!(specialized.ops[1], CodeOp::LoadConst(42));
    }

    #[test]
    fn peephole_fold() {
        let expr = CompiledExpr::new(vec![
            CodeOp::LoadConst(3),
            CodeOp::LoadConst(7),
            CodeOp::Add,
            CodeOp::LoadCol(0),
            CodeOp::Mul,
        ], 0);
        let folded = RuntimeSpecializer::peephole_fold(&expr);
        // 3+7=10 → LoadConst(10), LoadCol(0), Mul
        assert_eq!(folded.ops.len(), 3);
        assert_eq!(folded.ops[0], CodeOp::LoadConst(10));
    }

    #[test]
    fn recompilation_tracker() {
        let mut tracker = RecompilationTracker::new(Duration::from_millis(50), 3);
        tracker.record("q1", Duration::from_millis(100));
        tracker.record("q1", Duration::from_millis(100));
        assert!(!tracker.should_recompile("q1")); // only 2 executions
        tracker.record("q1", Duration::from_millis(100));
        assert!(tracker.should_recompile("q1")); // avg > 50ms
        tracker.mark_recompiled("q1");
        assert!(!tracker.should_recompile("q1")); // reset
    }
}
