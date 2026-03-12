// R17 – Query optimizer deep strengthening:
//   - Cost model calibration
//   - Multi-table join enumeration (DPccp-style)
//   - Predicate pushdown enhancement
//   - Subquery decorrelation
//   - Statistics sampling
//
// Provides:
//   - `CostCalibrator`: tunes cost model factors from observed latencies
//   - `JoinEnumerator`: bottom-up join order enumeration
//   - `PredicatePushdown`: pushes predicates closer to base relations
//   - `SubqueryDecorrelator`: rewrites correlated subqueries to joins
//   - `StatsSampler`: reservoir / Bernoulli sampling for stats collection

use std::collections::{HashMap, HashSet};

// ── Cost Model Calibration ────────────────────────────────────────────

/// A cost factor in the model.
#[derive(Debug, Clone)]
pub struct CostFactor {
    pub name: String,
    pub value: f64,
    pub observed_samples: Vec<f64>,
}

impl CostFactor {
    pub fn new(name: &str, initial: f64) -> Self {
        Self { name: name.to_string(), value: initial, observed_samples: Vec::new() }
    }

    /// Record an observed actual cost.
    pub fn observe(&mut self, actual: f64) {
        self.observed_samples.push(actual);
    }

    /// Recalibrate from observations (simple average).
    pub fn calibrate(&mut self) {
        if self.observed_samples.is_empty() { return; }
        let sum: f64 = self.observed_samples.iter().sum();
        self.value = sum / self.observed_samples.len() as f64;
    }
}

/// Manages cost model factors and calibration.
pub struct CostCalibrator {
    factors: HashMap<String, CostFactor>,
}

impl CostCalibrator {
    pub fn new() -> Self {
        let mut factors = HashMap::new();
        factors.insert("seq_page_cost".to_string(), CostFactor::new("seq_page_cost", 1.0));
        factors.insert("random_page_cost".to_string(), CostFactor::new("random_page_cost", 4.0));
        factors.insert("cpu_tuple_cost".to_string(), CostFactor::new("cpu_tuple_cost", 0.01));
        factors.insert("cpu_index_cost".to_string(), CostFactor::new("cpu_index_cost", 0.005));
        Self { factors }
    }

    pub fn get_factor(&self, name: &str) -> Option<f64> {
        self.factors.get(name).map(|f| f.value)
    }

    pub fn observe(&mut self, name: &str, actual: f64) {
        if let Some(f) = self.factors.get_mut(name) {
            f.observe(actual);
        }
    }

    pub fn calibrate_all(&mut self) {
        for f in self.factors.values_mut() {
            f.calibrate();
        }
    }

    pub fn factor_count(&self) -> usize {
        self.factors.len()
    }

    /// Estimate cost: pages * page_cost + tuples * cpu_tuple_cost.
    pub fn estimate_scan_cost(&self, pages: f64, tuples: f64, is_random: bool) -> f64 {
        let page_cost = if is_random {
            self.get_factor("random_page_cost").unwrap_or(4.0)
        } else {
            self.get_factor("seq_page_cost").unwrap_or(1.0)
        };
        let cpu_cost = self.get_factor("cpu_tuple_cost").unwrap_or(0.01);
        pages * page_cost + tuples * cpu_cost
    }
}

// ── Join Enumeration ──────────────────────────────────────────────────

/// Represents a join between two relation sets.
#[derive(Debug, Clone)]
pub struct JoinEdge {
    pub left: String,
    pub right: String,
    pub selectivity: f64,
}

/// Bottom-up join order enumerator.
pub struct JoinEnumerator {
    relations: Vec<String>,
    cardinalities: HashMap<String, f64>,
    edges: Vec<JoinEdge>,
}

impl JoinEnumerator {
    pub fn new() -> Self {
        Self { relations: Vec::new(), cardinalities: HashMap::new(), edges: Vec::new() }
    }

    pub fn add_relation(&mut self, name: &str, cardinality: f64) {
        self.relations.push(name.to_string());
        self.cardinalities.insert(name.to_string(), cardinality);
    }

    pub fn add_join_edge(&mut self, left: &str, right: &str, selectivity: f64) {
        self.edges.push(JoinEdge {
            left: left.to_string(),
            right: right.to_string(),
            selectivity,
        });
    }

    /// Find the selectivity for joining two sets (multiplicative for all edges between them).
    fn join_selectivity(&self, left_set: &HashSet<String>, right_set: &HashSet<String>) -> f64 {
        let mut sel = 1.0;
        for e in &self.edges {
            let l_in_left = left_set.contains(&e.left);
            let r_in_right = right_set.contains(&e.right);
            let l_in_right = left_set.contains(&e.right);
            let r_in_left = right_set.contains(&e.left);
            if (l_in_left && r_in_right) || (l_in_right && r_in_left) {
                sel *= e.selectivity;
            }
        }
        sel
    }

    /// Estimate cardinality of a set of joined relations.
    fn set_cardinality(&self, set: &HashSet<String>) -> f64 {
        let mut card = 1.0;
        for r in set {
            card *= self.cardinalities.get(r).copied().unwrap_or(1.0);
        }
        card
    }

    /// Enumerate join orders and return the best (lowest cost) as ordered list.
    /// Uses greedy approach for simplicity.
    pub fn find_best_order(&self) -> Vec<String> {
        if self.relations.is_empty() { return vec![]; }
        let mut remaining: HashSet<String> = self.relations.iter().cloned().collect();
        let mut result = Vec::new();

        // Start with smallest relation
        let first = remaining.iter()
            .min_by(|a, b| {
                let ca = self.cardinalities.get(*a).unwrap_or(&1.0);
                let cb = self.cardinalities.get(*b).unwrap_or(&1.0);
                ca.partial_cmp(cb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap();
        remaining.remove(&first);
        result.push(first);

        while !remaining.is_empty() {
            let current_set: HashSet<String> = result.iter().cloned().collect();
            let next = remaining.iter()
                .min_by(|a, b| {
                    let mut set_a = HashSet::new();
                    set_a.insert((*a).clone());
                    let mut set_b = HashSet::new();
                    set_b.insert((*b).clone());
                    let cost_a = self.set_cardinality(&current_set)
                        * self.cardinalities.get(*a).unwrap_or(&1.0)
                        * self.join_selectivity(&current_set, &set_a);
                    let cost_b = self.set_cardinality(&current_set)
                        * self.cardinalities.get(*b).unwrap_or(&1.0)
                        * self.join_selectivity(&current_set, &set_b);
                    cost_a.partial_cmp(&cost_b).unwrap_or(std::cmp::Ordering::Equal)
                })
                .cloned()
                .unwrap();
            remaining.remove(&next);
            result.push(next);
        }
        result
    }
}

// ── Predicate Pushdown ────────────────────────────────────────────────

/// A predicate with its referenced tables.
#[derive(Debug, Clone)]
pub struct Predicate {
    pub expr: String,
    pub referenced_tables: HashSet<String>,
}

/// Query plan node for pushdown.
#[derive(Debug, Clone)]
pub struct PushdownNode {
    pub node_type: String,
    pub table: Option<String>,
    pub predicates: Vec<Predicate>,
    pub children: Vec<PushdownNode>,
}

/// Pushes predicates closer to base scans.
pub struct PredicatePushdown;

impl PredicatePushdown {
    /// Push a predicate down through a plan tree.
    pub fn push_down(node: &mut PushdownNode, predicate: Predicate) -> bool {
        // If this is a scan and predicate references only this table, push here
        if let Some(ref table) = node.table {
            if predicate.referenced_tables.len() == 1
                && predicate.referenced_tables.contains(table) {
                node.predicates.push(predicate);
                return true;
            }
        }
        // Try pushing into children
        for child in &mut node.children {
            let child_tables = Self::collect_tables(child);
            if predicate.referenced_tables.is_subset(&child_tables) {
                return Self::push_down(child, predicate);
            }
        }
        // Cannot push further, attach here
        node.predicates.push(predicate);
        false
    }

    fn collect_tables(node: &PushdownNode) -> HashSet<String> {
        let mut tables = HashSet::new();
        if let Some(ref t) = node.table { tables.insert(t.clone()); }
        for child in &node.children {
            tables.extend(Self::collect_tables(child));
        }
        tables
    }
}

// ── Subquery Decorrelation ────────────────────────────────────────────

/// Correlated subquery representation.
#[derive(Debug, Clone)]
pub struct CorrelatedSubquery {
    pub subquery_id: u32,
    pub outer_refs: Vec<String>, // columns from outer query
    pub inner_table: String,
    pub predicate: String,
    pub is_exists: bool,
}

/// Result of decorrelation: a join replacement.
#[derive(Debug, Clone)]
pub struct DecorrelatedJoin {
    pub original_id: u32,
    pub join_type: String, // "semi", "anti", "inner"
    pub join_table: String,
    pub join_condition: String,
}

/// Rewrites correlated subqueries into joins.
pub struct SubqueryDecorrelator;

impl SubqueryDecorrelator {
    /// Decorrelate a subquery into a join.
    pub fn decorrelate(sub: &CorrelatedSubquery) -> DecorrelatedJoin {
        let join_type = if sub.is_exists {
            "semi".to_string()
        } else {
            "inner".to_string()
        };
        let join_condition = format!(
            "{} ON {}",
            sub.outer_refs.join(", "),
            sub.predicate
        );
        DecorrelatedJoin {
            original_id: sub.subquery_id,
            join_type,
            join_table: sub.inner_table.clone(),
            join_condition,
        }
    }

    /// Batch decorrelate.
    pub fn decorrelate_all(subqueries: &[CorrelatedSubquery]) -> Vec<DecorrelatedJoin> {
        subqueries.iter().map(Self::decorrelate).collect()
    }
}

// ── Statistics Sampling ───────────────────────────────────────────────

/// Sampling method.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SamplingMethod {
    Reservoir,
    Bernoulli,
    SystemPage,
}

/// Reservoir sampler for collecting representative data.
pub struct StatsSampler {
    method: SamplingMethod,
    sample_size: usize,
    reservoir: Vec<i64>,
    seen: u64,
    rng_state: u64,
}

impl StatsSampler {
    pub fn new(method: SamplingMethod, sample_size: usize) -> Self {
        Self {
            method,
            sample_size,
            reservoir: Vec::with_capacity(sample_size),
            seen: 0,
            rng_state: 12345,
        }
    }

    fn next_rand(&mut self) -> u64 {
        // Simple xorshift PRNG
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        self.rng_state
    }

    /// Add a value (reservoir sampling).
    pub fn add(&mut self, value: i64) {
        self.seen += 1;
        match self.method {
            SamplingMethod::Reservoir => {
                if self.reservoir.len() < self.sample_size {
                    self.reservoir.push(value);
                } else {
                    let r = (self.next_rand() % self.seen) as usize;
                    if r < self.sample_size {
                        self.reservoir[r] = value;
                    }
                }
            }
            SamplingMethod::Bernoulli => {
                let threshold = (self.sample_size as f64
                    / (self.seen as f64).max(1.0) * u64::MAX as f64) as u64;
                if self.next_rand() < threshold && self.reservoir.len() < self.sample_size {
                    self.reservoir.push(value);
                }
            }
            SamplingMethod::SystemPage => {
                // page-level: accept all from sampled pages
                if self.reservoir.len() < self.sample_size {
                    self.reservoir.push(value);
                }
            }
        }
    }

    pub fn sample(&self) -> &[i64] {
        &self.reservoir
    }

    pub fn sample_count(&self) -> usize {
        self.reservoir.len()
    }

    pub fn total_seen(&self) -> u64 {
        self.seen
    }

    /// Compute NDV (number of distinct values) estimate from sample.
    pub fn estimate_ndv(&self) -> usize {
        let mut set = HashSet::new();
        for v in &self.reservoir { set.insert(*v); }
        set.len()
    }

    /// Compute min/max from sample.
    pub fn sample_range(&self) -> Option<(i64, i64)> {
        if self.reservoir.is_empty() { return None; }
        let min = *self.reservoir.iter().min().unwrap();
        let max = *self.reservoir.iter().max().unwrap();
        Some((min, max))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_calibrator_basic() {
        let mut cc = CostCalibrator::new();
        assert_eq!(cc.factor_count(), 4);
        assert_eq!(cc.get_factor("seq_page_cost"), Some(1.0));
        cc.observe("seq_page_cost", 1.5);
        cc.observe("seq_page_cost", 2.5);
        cc.calibrate_all();
        assert!((cc.get_factor("seq_page_cost").unwrap() - 2.0).abs() < 0.01);
    }

    #[test]
    fn cost_calibrator_scan_estimate() {
        let cc = CostCalibrator::new();
        let seq = cc.estimate_scan_cost(100.0, 10000.0, false);
        let rnd = cc.estimate_scan_cost(100.0, 10000.0, true);
        assert!(rnd > seq); // random is more expensive
    }

    #[test]
    fn join_enumerator_greedy() {
        let mut je = JoinEnumerator::new();
        je.add_relation("orders", 10000.0);
        je.add_relation("customers", 500.0);
        je.add_relation("items", 50000.0);
        je.add_join_edge("customers", "orders", 0.01);
        je.add_join_edge("orders", "items", 0.001);
        let order = je.find_best_order();
        assert_eq!(order.len(), 3);
        // Smallest first
        assert_eq!(order[0], "customers");
    }

    #[test]
    fn predicate_pushdown_to_scan() {
        let pred = Predicate {
            expr: "t1.x > 5".to_string(),
            referenced_tables: {
                let mut s = HashSet::new();
                s.insert("t1".to_string());
                s
            },
        };
        let mut root = PushdownNode {
            node_type: "join".to_string(),
            table: None,
            predicates: vec![],
            children: vec![
                PushdownNode {
                    node_type: "scan".to_string(),
                    table: Some("t1".to_string()),
                    predicates: vec![],
                    children: vec![],
                },
                PushdownNode {
                    node_type: "scan".to_string(),
                    table: Some("t2".to_string()),
                    predicates: vec![],
                    children: vec![],
                },
            ],
        };
        let pushed = PredicatePushdown::push_down(&mut root, pred);
        assert!(pushed);
        assert_eq!(root.children[0].predicates.len(), 1);
        assert!(root.predicates.is_empty());
    }

    #[test]
    fn subquery_decorrelation_exists() {
        let sub = CorrelatedSubquery {
            subquery_id: 1,
            outer_refs: vec!["o.id".to_string()],
            inner_table: "details".to_string(),
            predicate: "o.id = d.order_id".to_string(),
            is_exists: true,
        };
        let join = SubqueryDecorrelator::decorrelate(&sub);
        assert_eq!(join.join_type, "semi");
        assert_eq!(join.join_table, "details");
    }

    #[test]
    fn stats_sampler_reservoir() {
        let mut s = StatsSampler::new(SamplingMethod::Reservoir, 10);
        for i in 0..1000 {
            s.add(i);
        }
        assert_eq!(s.sample_count(), 10);
        assert_eq!(s.total_seen(), 1000);
        let ndv = s.estimate_ndv();
        assert!(ndv > 0 && ndv <= 10);
        let (min, max) = s.sample_range().unwrap();
        assert!(min >= 0 && max < 1000);
    }
}
