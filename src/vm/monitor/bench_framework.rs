// R19 – Benchmark framework
//
// Provides:
//   - `BenchResult`: single benchmark measurement
//   - `BenchSuite`: collection of benchmarks with timing
//   - `BenchReporter`: formats results for display
//   - `MicroBench`: lightweight micro-benchmark runner (no external deps)

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Result of a single benchmark run.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub iterations: u64,
    pub total_duration: Duration,
    pub min_ns: u64,
    pub max_ns: u64,
    pub avg_ns: u64,
    pub p50_ns: u64,
    pub p99_ns: u64,
}

impl BenchResult {
    pub fn ops_per_sec(&self) -> f64 {
        if self.avg_ns == 0 {
            return 0.0;
        }
        1_000_000_000.0 / self.avg_ns as f64
    }

    pub fn avg_us(&self) -> f64 {
        self.avg_ns as f64 / 1000.0
    }
}

/// A benchmark function.
pub struct BenchFn {
    pub name: String,
    pub func: Box<dyn Fn(u64)>,
    pub warmup_iters: u64,
    pub bench_iters: u64,
}

/// Benchmark suite.
pub struct BenchSuite {
    benchmarks: Vec<BenchFn>,
    results: Vec<BenchResult>,
}

impl Default for BenchSuite {
    fn default() -> Self {
        Self::new()
    }
}

impl BenchSuite {
    pub fn new() -> Self {
        Self {
            benchmarks: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Register a benchmark with a closure.
    pub fn add<F: Fn(u64) + 'static>(&mut self, name: &str, warmup: u64, iters: u64, func: F) {
        self.benchmarks.push(BenchFn {
            name: name.to_string(),
            func: Box::new(func),
            warmup_iters: warmup,
            bench_iters: iters,
        });
    }

    /// Run all benchmarks.
    pub fn run_all(&mut self) {
        self.results.clear();
        for bench in &self.benchmarks {
            let result = MicroBench::run(
                &bench.name,
                bench.warmup_iters,
                bench.bench_iters,
                &bench.func,
            );
            self.results.push(result);
        }
    }

    pub fn results(&self) -> &[BenchResult] {
        &self.results
    }

    pub fn bench_count(&self) -> usize {
        self.benchmarks.len()
    }
}

/// Lightweight micro-benchmark runner.
pub struct MicroBench;

impl MicroBench {
    /// Run a benchmark function and collect timing data.
    pub fn run<F: Fn(u64)>(name: &str, warmup: u64, iterations: u64, func: &F) -> BenchResult {
        // Warmup
        for i in 0..warmup {
            func(i);
        }

        // Measure
        let mut timings = Vec::with_capacity(iterations as usize);
        let start_total = Instant::now();

        for i in 0..iterations {
            let start = Instant::now();
            func(i);
            let elapsed = start.elapsed().as_nanos() as u64;
            timings.push(elapsed);
        }

        let total_duration = start_total.elapsed();

        // Sort for percentiles
        timings.sort();

        let min_ns = *timings.first().unwrap_or(&0);
        let max_ns = *timings.last().unwrap_or(&0);
        let avg_ns = if timings.is_empty() {
            0
        } else {
            timings.iter().sum::<u64>() / timings.len() as u64
        };
        let p50_ns = Self::percentile(&timings, 50);
        let p99_ns = Self::percentile(&timings, 99);

        BenchResult {
            name: name.to_string(),
            iterations,
            total_duration,
            min_ns,
            max_ns,
            avg_ns,
            p50_ns,
            p99_ns,
        }
    }

    fn percentile(sorted: &[u64], pct: u32) -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((pct as f64 / 100.0) * (sorted.len() - 1) as f64) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
}

/// Formats benchmark results for display.
pub struct BenchReporter;

impl BenchReporter {
    /// Format results as a text table.
    pub fn format_table(results: &[BenchResult]) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<30} {:>10} {:>10} {:>10} {:>10} {:>12}",
            "Benchmark", "Avg(μs)", "P50(μs)", "P99(μs)", "Min(μs)", "Ops/s"
        ));
        lines.push("-".repeat(84));

        for r in results {
            lines.push(format!(
                "{:<30} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>12.0}",
                r.name,
                r.avg_us(),
                r.p50_ns as f64 / 1000.0,
                r.p99_ns as f64 / 1000.0,
                r.min_ns as f64 / 1000.0,
                r.ops_per_sec(),
            ));
        }
        lines.join("\n")
    }

    /// Compare two sets of results (e.g., before/after optimization).
    pub fn compare(baseline: &[BenchResult], current: &[BenchResult]) -> Vec<BenchComparison> {
        let baseline_map: HashMap<String, &BenchResult> =
            baseline.iter().map(|r| (r.name.clone(), r)).collect();

        current
            .iter()
            .filter_map(|cur| {
                baseline_map.get(&cur.name).map(|base| {
                    let speedup = if cur.avg_ns > 0 {
                        base.avg_ns as f64 / cur.avg_ns as f64
                    } else {
                        0.0
                    };
                    BenchComparison {
                        name: cur.name.clone(),
                        baseline_avg_ns: base.avg_ns,
                        current_avg_ns: cur.avg_ns,
                        speedup,
                    }
                })
            })
            .collect()
    }
}

/// Comparison between baseline and current benchmark.
#[derive(Debug, Clone)]
pub struct BenchComparison {
    pub name: String,
    pub baseline_avg_ns: u64,
    pub current_avg_ns: u64,
    pub speedup: f64,
}

impl BenchComparison {
    pub fn is_regression(&self) -> bool {
        self.speedup < 0.95
    }

    pub fn is_improvement(&self) -> bool {
        self.speedup > 1.05
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micro_bench_basic() {
        let result = MicroBench::run("noop", 10, 100, &|_| {});
        assert_eq!(result.iterations, 100);
        assert!(result.avg_ns < 1_000_000); // should be very fast
        assert!(result.ops_per_sec() > 100.0);
    }

    #[test]
    fn bench_result_ops() {
        let r = BenchResult {
            name: "test".to_string(),
            iterations: 1000,
            total_duration: Duration::from_millis(100),
            min_ns: 50,
            max_ns: 200,
            avg_ns: 100,
            p50_ns: 90,
            p99_ns: 180,
        };
        assert!((r.ops_per_sec() - 10_000_000.0).abs() < 1.0);
        assert!((r.avg_us() - 0.1).abs() < 0.001);
    }

    #[test]
    fn bench_suite_lifecycle() {
        let mut suite = BenchSuite::new();
        suite.add("add_ints", 5, 50, |i| {
            let _ = i + 1;
        });
        suite.add("multiply", 5, 50, |i| {
            let _ = i * 2;
        });
        assert_eq!(suite.bench_count(), 2);
        suite.run_all();
        assert_eq!(suite.results().len(), 2);
    }

    #[test]
    fn bench_reporter_table() {
        let results = vec![BenchResult {
            name: "insert_row".to_string(),
            iterations: 1000,
            total_duration: Duration::from_millis(50),
            min_ns: 40_000,
            max_ns: 100_000,
            avg_ns: 50_000,
            p50_ns: 48_000,
            p99_ns: 95_000,
        }];
        let table = BenchReporter::format_table(&results);
        assert!(table.contains("insert_row"));
        assert!(table.contains("Avg"));
    }

    #[test]
    fn bench_comparison() {
        let baseline = vec![BenchResult {
            name: "query".into(),
            iterations: 100,
            total_duration: Duration::from_millis(10),
            min_ns: 80_000,
            max_ns: 120_000,
            avg_ns: 100_000,
            p50_ns: 95_000,
            p99_ns: 115_000,
        }];
        let current = vec![BenchResult {
            name: "query".into(),
            iterations: 100,
            total_duration: Duration::from_millis(5),
            min_ns: 40_000,
            max_ns: 60_000,
            avg_ns: 50_000,
            p50_ns: 48_000,
            p99_ns: 58_000,
        }];
        let cmp = BenchReporter::compare(&baseline, &current);
        assert_eq!(cmp.len(), 1);
        assert!(cmp[0].is_improvement());
        assert!(!cmp[0].is_regression());
        assert!((cmp[0].speedup - 2.0).abs() < 0.01);
    }

    #[test]
    fn percentile_edge_cases() {
        let r = MicroBench::run("single_iter", 0, 1, &|_| {});
        assert_eq!(r.iterations, 1);
        assert!(r.p50_ns >= r.min_ns);
        assert!(r.p99_ns <= r.max_ns);
    }
}
