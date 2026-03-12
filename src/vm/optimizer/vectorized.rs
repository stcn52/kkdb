// R13 – Vectorized execution engine + Pipeline execution model + JIT expression stub.
//
// Provides:
//   - `ColumnBatch`: columnar data batch for vectorized processing
//   - `VectorOp`: vectorized operations on column batches (filter, project, aggregate)
//   - `PipelineStage`: stage in a pipelined execution plan
//   - `Pipeline`: chain of stages for push-based execution
//   - `JitExprCompiler`: stub for expression compilation (pattern matching optimizer)

use std::collections::HashMap;

// ── Column Batch ──────────────────────────────────────────────────────

/// A columnar data batch — columns of values for vectorized processing.
#[derive(Debug, Clone)]
pub struct ColumnBatch {
    /// Column data: column_index → values.
    pub columns: Vec<Vec<i64>>,
    /// Number of rows.
    pub row_count: usize,
    /// Column names (for reference).
    pub col_names: Vec<String>,
}

impl ColumnBatch {
    /// Create an empty batch with the given column names.
    pub fn new(col_names: Vec<String>) -> Self {
        let ncols = col_names.len();
        Self {
            columns: vec![Vec::new(); ncols],
            row_count: 0,
            col_names,
        }
    }

    /// Create a batch from row-oriented data (i64 only, for simplicity).
    pub fn from_rows(col_names: Vec<String>, rows: &[Vec<i64>]) -> Self {
        let ncols = col_names.len();
        let mut columns = vec![Vec::with_capacity(rows.len()); ncols];
        for row in rows {
            for (i, &val) in row.iter().enumerate() {
                if i < ncols {
                    columns[i].push(val);
                }
            }
        }
        Self {
            row_count: rows.len(),
            columns,
            col_names,
        }
    }

    /// Add a row.
    pub fn push_row(&mut self, row: &[i64]) {
        for (i, &val) in row.iter().enumerate() {
            if i < self.columns.len() {
                self.columns[i].push(val);
            }
        }
        self.row_count += 1;
    }

    /// Get a column by index.
    pub fn column(&self, idx: usize) -> Option<&[i64]> {
        self.columns.get(idx).map(|v| v.as_slice())
    }

    /// Get a column by name.
    pub fn column_by_name(&self, name: &str) -> Option<&[i64]> {
        self.col_names
            .iter()
            .position(|n| n == name)
            .and_then(|idx| self.column(idx))
    }

    /// Number of columns.
    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// Materialize back to row-oriented format.
    pub fn to_rows(&self) -> Vec<Vec<i64>> {
        let mut rows = Vec::with_capacity(self.row_count);
        for i in 0..self.row_count {
            let mut row = Vec::with_capacity(self.columns.len());
            for col in &self.columns {
                row.push(col[i]);
            }
            rows.push(row);
        }
        rows
    }
}

// ── Vectorized Operations ─────────────────────────────────────────────

/// Vectorized operations on column batches.
pub struct VectorOp;

impl VectorOp {
    /// Filter: keep only rows where column[col_idx] satisfies the predicate.
    pub fn filter(batch: &ColumnBatch, col_idx: usize, pred: impl Fn(i64) -> bool) -> ColumnBatch {
        let col = match batch.column(col_idx) {
            Some(c) => c,
            None => return ColumnBatch::new(batch.col_names.clone()),
        };
        // Build selection vector
        let sel: Vec<usize> = col
            .iter()
            .enumerate()
            .filter(|(_, &v)| pred(v))
            .map(|(i, _)| i)
            .collect();

        let mut result = ColumnBatch::new(batch.col_names.clone());
        for &row_idx in &sel {
            let row: Vec<i64> = batch.columns.iter().map(|c| c[row_idx]).collect();
            result.push_row(&row);
        }
        result
    }

    /// Project: select only the specified column indices.
    pub fn project(batch: &ColumnBatch, col_indices: &[usize]) -> ColumnBatch {
        let names: Vec<String> = col_indices
            .iter()
            .filter_map(|&i| batch.col_names.get(i).cloned())
            .collect();
        let columns: Vec<Vec<i64>> = col_indices
            .iter()
            .filter_map(|&i| batch.columns.get(i).cloned())
            .collect();
        ColumnBatch {
            row_count: batch.row_count,
            columns,
            col_names: names,
        }
    }

    /// Aggregate: compute SUM for a column.
    pub fn sum(batch: &ColumnBatch, col_idx: usize) -> Option<i64> {
        batch.column(col_idx).map(|c| c.iter().sum())
    }

    /// Aggregate: compute COUNT for a column.
    pub fn count(batch: &ColumnBatch) -> usize {
        batch.row_count
    }

    /// Aggregate: compute MIN for a column.
    pub fn min(batch: &ColumnBatch, col_idx: usize) -> Option<i64> {
        batch.column(col_idx).and_then(|c| c.iter().copied().min())
    }

    /// Aggregate: compute MAX for a column.
    pub fn max(batch: &ColumnBatch, col_idx: usize) -> Option<i64> {
        batch.column(col_idx).and_then(|c| c.iter().copied().max())
    }

    /// Element-wise addition of two columns (same batch), producing a new column.
    pub fn add_columns(batch: &ColumnBatch, col_a: usize, col_b: usize) -> Option<Vec<i64>> {
        let a = batch.column(col_a)?;
        let b = batch.column(col_b)?;
        Some(a.iter().zip(b.iter()).map(|(&x, &y)| x + y).collect())
    }

    /// Hash-aggregate: GROUP BY col_idx, SUM(agg_col_idx).
    pub fn hash_aggregate(
        batch: &ColumnBatch,
        group_col: usize,
        agg_col: usize,
    ) -> HashMap<i64, i64> {
        let mut map: HashMap<i64, i64> = HashMap::new();
        if let (Some(keys), Some(vals)) = (batch.column(group_col), batch.column(agg_col)) {
            for (&k, &v) in keys.iter().zip(vals.iter()) {
                *map.entry(k).or_insert(0) += v;
            }
        }
        map
    }
}

// ── Pipeline Execution ────────────────────────────────────────────────

/// A stage in a pipelined execution plan.
#[derive(Debug, Clone)]
pub enum PipelineStage {
    /// Full table scan producing a batch.
    Scan { table: String },
    /// Filter rows.
    Filter {
        col_idx: usize,
        op: FilterOp,
        value: i64,
    },
    /// Project columns.
    Project { col_indices: Vec<usize> },
    /// Aggregate.
    Aggregate { agg_type: AggType, col_idx: usize },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl FilterOp {
    pub fn apply(&self, value: i64, target: i64) -> bool {
        match self {
            FilterOp::Eq => value == target,
            FilterOp::Ne => value != target,
            FilterOp::Lt => value < target,
            FilterOp::Le => value <= target,
            FilterOp::Gt => value > target,
            FilterOp::Ge => value >= target,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AggType {
    Sum,
    Count,
    Min,
    Max,
}

/// A pipeline of stages to execute in sequence.
pub struct Pipeline {
    stages: Vec<PipelineStage>,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn add_stage(&mut self, stage: PipelineStage) {
        self.stages.push(stage);
    }

    pub fn stages(&self) -> &[PipelineStage] {
        &self.stages
    }

    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }

    /// Execute the pipeline on an input batch (scan is simulated by the caller).
    pub fn execute(&self, input: ColumnBatch) -> PipelineResult {
        let mut batch = input;

        for stage in &self.stages {
            match stage {
                PipelineStage::Scan { .. } => {
                    // Scan is handled externally; skip
                }
                PipelineStage::Filter { col_idx, op, value } => {
                    let target = *value;
                    let filter_op = *op;
                    batch = VectorOp::filter(&batch, *col_idx, |v| filter_op.apply(v, target));
                }
                PipelineStage::Project { col_indices } => {
                    batch = VectorOp::project(&batch, col_indices);
                }
                PipelineStage::Aggregate { agg_type, col_idx } => {
                    let result = match agg_type {
                        AggType::Sum => VectorOp::sum(&batch, *col_idx).unwrap_or(0),
                        AggType::Count => batch.row_count as i64,
                        AggType::Min => VectorOp::min(&batch, *col_idx).unwrap_or(0),
                        AggType::Max => VectorOp::max(&batch, *col_idx).unwrap_or(0),
                    };
                    return PipelineResult::Scalar(result);
                }
            }
        }

        PipelineResult::Batch(batch)
    }
}

/// Result of pipeline execution.
#[derive(Debug)]
pub enum PipelineResult {
    Batch(ColumnBatch),
    Scalar(i64),
}

// ── JIT Expression Compiler (stub) ───────────────────────────────────

/// Pattern-matching expression optimizer (JIT stub).
///
/// In a real system this would compile expressions to native code.
/// Here we provide expression pattern recognition for optimization.
#[derive(Debug, Clone)]
pub enum ExprPattern {
    /// Constant value.
    Const(i64),
    /// Column reference.
    ColRef(usize),
    /// Binary operation.
    BinOp {
        op: BinOpKind,
        left: Box<ExprPattern>,
        right: Box<ExprPattern>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
}

impl ExprPattern {
    /// Evaluate the expression for a given row (column values).
    pub fn eval(&self, row: &[i64]) -> i64 {
        match self {
            ExprPattern::Const(v) => *v,
            ExprPattern::ColRef(idx) => row.get(*idx).copied().unwrap_or(0),
            ExprPattern::BinOp { op, left, right } => {
                let l = left.eval(row);
                let r = right.eval(row);
                match op {
                    BinOpKind::Add => l + r,
                    BinOpKind::Sub => l - r,
                    BinOpKind::Mul => l * r,
                    BinOpKind::Div => {
                        if r != 0 {
                            l / r
                        } else {
                            0
                        }
                    }
                }
            }
        }
    }

    /// Apply this expression to every row in the batch, producing a new column.
    pub fn eval_batch(&self, batch: &ColumnBatch) -> Vec<i64> {
        let rows = batch.to_rows();
        rows.iter().map(|row| self.eval(row)).collect()
    }

    /// Check if the expression is a constant (can be folded).
    pub fn is_constant(&self) -> bool {
        matches!(self, ExprPattern::Const(_))
    }

    /// Constant fold: if both sides of a BinOp are Const, reduce to Const.
    pub fn constant_fold(self) -> Self {
        match self {
            ExprPattern::BinOp { op, left, right } => {
                let l = left.constant_fold();
                let r = right.constant_fold();
                if let (ExprPattern::Const(lv), ExprPattern::Const(rv)) = (&l, &r) {
                    let result = match op {
                        BinOpKind::Add => lv + rv,
                        BinOpKind::Sub => lv - rv,
                        BinOpKind::Mul => lv * rv,
                        BinOpKind::Div => {
                            if *rv != 0 {
                                lv / rv
                            } else {
                                0
                            }
                        }
                    };
                    ExprPattern::Const(result)
                } else {
                    ExprPattern::BinOp {
                        op,
                        left: Box::new(l),
                        right: Box::new(r),
                    }
                }
            }
            other => other,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_batch_from_rows() {
        let batch = ColumnBatch::from_rows(
            vec!["a".into(), "b".into()],
            &[vec![1, 10], vec![2, 20], vec![3, 30]],
        );
        assert_eq!(batch.row_count, 3);
        assert_eq!(batch.column(0), Some([1, 2, 3].as_slice()));
        assert_eq!(batch.column(1), Some([10, 20, 30].as_slice()));
    }

    #[test]
    fn column_batch_by_name() {
        let batch = ColumnBatch::from_rows(vec!["x".into(), "y".into()], &[vec![5, 6]]);
        assert_eq!(batch.column_by_name("y"), Some([6].as_slice()));
        assert_eq!(batch.column_by_name("z"), None);
    }

    #[test]
    fn vector_filter() {
        let batch = ColumnBatch::from_rows(
            vec!["id".into(), "val".into()],
            &[vec![1, 10], vec![2, 20], vec![3, 30]],
        );
        let filtered = VectorOp::filter(&batch, 1, |v| v > 15);
        assert_eq!(filtered.row_count, 2);
        assert_eq!(filtered.column(0), Some([2, 3].as_slice()));
    }

    #[test]
    fn vector_project() {
        let batch =
            ColumnBatch::from_rows(vec!["a".into(), "b".into(), "c".into()], &[vec![1, 2, 3]]);
        let projected = VectorOp::project(&batch, &[0, 2]);
        assert_eq!(projected.num_columns(), 2);
        assert_eq!(projected.col_names, vec!["a", "c"]);
    }

    #[test]
    fn vector_aggregates() {
        let batch = ColumnBatch::from_rows(vec!["v".into()], &[vec![10], vec![20], vec![30]]);
        assert_eq!(VectorOp::sum(&batch, 0), Some(60));
        assert_eq!(VectorOp::count(&batch), 3);
        assert_eq!(VectorOp::min(&batch, 0), Some(10));
        assert_eq!(VectorOp::max(&batch, 0), Some(30));
    }

    #[test]
    fn vector_add_columns() {
        let batch =
            ColumnBatch::from_rows(vec!["a".into(), "b".into()], &[vec![1, 10], vec![2, 20]]);
        let result = VectorOp::add_columns(&batch, 0, 1).unwrap();
        assert_eq!(result, vec![11, 22]);
    }

    #[test]
    fn vector_hash_aggregate() {
        let batch = ColumnBatch::from_rows(
            vec!["group".into(), "val".into()],
            &[vec![1, 10], vec![1, 20], vec![2, 30]],
        );
        let agg = VectorOp::hash_aggregate(&batch, 0, 1);
        assert_eq!(agg[&1], 30);
        assert_eq!(agg[&2], 30);
    }

    #[test]
    fn pipeline_filter_project() {
        let batch = ColumnBatch::from_rows(
            vec!["id".into(), "val".into()],
            &[vec![1, 100], vec![2, 200], vec![3, 50]],
        );
        let mut pipe = Pipeline::new();
        pipe.add_stage(PipelineStage::Filter {
            col_idx: 1,
            op: FilterOp::Ge,
            value: 100,
        });
        pipe.add_stage(PipelineStage::Project {
            col_indices: vec![0],
        });
        match pipe.execute(batch) {
            PipelineResult::Batch(b) => {
                assert_eq!(b.row_count, 2);
                assert_eq!(b.column(0), Some([1, 2].as_slice()));
            }
            _ => panic!("expected batch"),
        }
    }

    #[test]
    fn pipeline_aggregate() {
        let batch = ColumnBatch::from_rows(vec!["v".into()], &[vec![10], vec![20], vec![30]]);
        let mut pipe = Pipeline::new();
        pipe.add_stage(PipelineStage::Aggregate {
            agg_type: AggType::Sum,
            col_idx: 0,
        });
        match pipe.execute(batch) {
            PipelineResult::Scalar(v) => assert_eq!(v, 60),
            _ => panic!("expected scalar"),
        }
    }

    #[test]
    fn filter_op_apply() {
        assert!(FilterOp::Eq.apply(5, 5));
        assert!(!FilterOp::Eq.apply(5, 6));
        assert!(FilterOp::Lt.apply(3, 5));
        assert!(FilterOp::Ge.apply(5, 5));
    }

    #[test]
    fn expr_pattern_eval() {
        let expr = ExprPattern::BinOp {
            op: BinOpKind::Add,
            left: Box::new(ExprPattern::ColRef(0)),
            right: Box::new(ExprPattern::Const(10)),
        };
        assert_eq!(expr.eval(&[5]), 15);
    }

    #[test]
    fn expr_pattern_constant_fold() {
        let expr = ExprPattern::BinOp {
            op: BinOpKind::Mul,
            left: Box::new(ExprPattern::Const(3)),
            right: Box::new(ExprPattern::Const(7)),
        };
        let folded = expr.constant_fold();
        assert!(folded.is_constant());
        assert_eq!(folded.eval(&[]), 21);
    }

    #[test]
    fn expr_pattern_eval_batch() {
        let batch = ColumnBatch::from_rows(
            vec!["a".into(), "b".into()],
            &[vec![1, 10], vec![2, 20], vec![3, 30]],
        );
        let expr = ExprPattern::BinOp {
            op: BinOpKind::Add,
            left: Box::new(ExprPattern::ColRef(0)),
            right: Box::new(ExprPattern::ColRef(1)),
        };
        let result = expr.eval_batch(&batch);
        assert_eq!(result, vec![11, 22, 33]);
    }
}
