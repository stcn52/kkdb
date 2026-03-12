// ── src/vm/engine/dev_experience.rs ──
// R22: 开发者体验与工具 — SQL Explain可视化 / 查询性能分析器 / Schema迁移工具 / 数据导入导出

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
// 1. ExplainVisualizer — SQL Explain 可视化
// ═══════════════════════════════════════════════════════════════════════

/// 执行计划节点
#[derive(Debug, Clone)]
pub struct PlanNode {
    pub id: usize,
    pub operator: String,
    pub table: Option<String>,
    pub estimated_rows: u64,
    pub estimated_cost: f64,
    pub actual_rows: Option<u64>,
    pub actual_time_us: Option<u64>,
    pub children: Vec<usize>,
}

/// Explain 可视化器
pub struct ExplainVisualizer {
    nodes: Vec<PlanNode>,
    total_cost: f64,
}

impl ExplainVisualizer {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            total_cost: 0.0,
        }
    }

    pub fn add_node(
        &mut self,
        operator: &str,
        table: Option<&str>,
        est_rows: u64,
        est_cost: f64,
        children: Vec<usize>,
    ) -> usize {
        let id = self.nodes.len();
        self.total_cost += est_cost;
        self.nodes.push(PlanNode {
            id,
            operator: operator.to_string(),
            table: table.map(|s| s.to_string()),
            estimated_rows: est_rows,
            estimated_cost: est_cost,
            actual_rows: None,
            actual_time_us: None,
            children,
        });
        id
    }

    pub fn set_actual(&mut self, node_id: usize, rows: u64, time_us: u64) {
        if let Some(n) = self.nodes.get_mut(node_id) {
            n.actual_rows = Some(rows);
            n.actual_time_us = Some(time_us);
        }
    }

    /// 生成树形文本表示
    pub fn render_tree(&self) -> String {
        if self.nodes.is_empty() {
            return String::from("(empty plan)");
        }
        let mut output = String::new();
        self.render_node(self.nodes.len() - 1, 0, &mut output);
        output
    }

    fn render_node(&self, node_id: usize, depth: usize, output: &mut String) {
        let indent = "  ".repeat(depth);
        if let Some(node) = self.nodes.get(node_id) {
            let table_str = node.table.as_deref().unwrap_or("-");
            let actual = match (node.actual_rows, node.actual_time_us) {
                (Some(r), Some(t)) => format!(" [actual: {} rows, {}us]", r, t),
                _ => String::new(),
            };
            output.push_str(&format!(
                "{}{} (table={}, est_rows={}, cost={:.1}){}\n",
                indent, node.operator, table_str, node.estimated_rows, node.estimated_cost, actual
            ));
            for &child_id in &node.children {
                self.render_node(child_id, depth + 1, output);
            }
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn total_cost(&self) -> f64 {
        self.total_cost
    }

    /// 找到代价最高的节点
    pub fn bottleneck(&self) -> Option<&PlanNode> {
        self.nodes.iter().max_by(|a, b| {
            a.estimated_cost
                .partial_cmp(&b.estimated_cost)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. QueryProfiler — 查询性能分析器
// ═══════════════════════════════════════════════════════════════════════

/// 查询性能快照
#[derive(Debug, Clone)]
pub struct QueryProfile {
    pub sql: String,
    pub parse_us: u64,
    pub optimize_us: u64,
    pub execute_us: u64,
    pub total_us: u64,
    pub rows_scanned: u64,
    pub rows_returned: u64,
    pub buffer_hits: u64,
    pub buffer_misses: u64,
    pub io_reads: u64,
}

impl QueryProfile {
    pub fn scan_efficiency(&self) -> f64 {
        if self.rows_scanned == 0 {
            return 1.0;
        }
        self.rows_returned as f64 / self.rows_scanned as f64
    }

    pub fn buffer_hit_rate(&self) -> f64 {
        let total = self.buffer_hits + self.buffer_misses;
        if total == 0 {
            return 1.0;
        }
        self.buffer_hits as f64 / total as f64
    }
}

/// 查询性能分析器
pub struct QueryProfiler {
    profiles: Vec<QueryProfile>,
    max_profiles: usize,
}

impl QueryProfiler {
    pub fn new(max_profiles: usize) -> Self {
        Self {
            profiles: Vec::new(),
            max_profiles,
        }
    }

    pub fn record(&mut self, profile: QueryProfile) {
        if self.profiles.len() >= self.max_profiles {
            self.profiles.remove(0);
        }
        self.profiles.push(profile);
    }

    /// 找到最慢的 N 个查询
    pub fn slowest(&self, n: usize) -> Vec<&QueryProfile> {
        let mut sorted: Vec<&QueryProfile> = self.profiles.iter().collect();
        sorted.sort_by(|a, b| b.total_us.cmp(&a.total_us));
        sorted.truncate(n);
        sorted
    }

    /// 找到扫描效率最低的查询
    pub fn least_efficient(&self, n: usize) -> Vec<&QueryProfile> {
        let mut sorted: Vec<&QueryProfile> = self.profiles.iter().collect();
        sorted.sort_by(|a, b| {
            a.scan_efficiency()
                .partial_cmp(&b.scan_efficiency())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted.truncate(n);
        sorted
    }

    pub fn profile_count(&self) -> usize {
        self.profiles.len()
    }

    pub fn avg_latency_us(&self) -> f64 {
        if self.profiles.is_empty() {
            return 0.0;
        }
        let total: u64 = self.profiles.iter().map(|p| p.total_us).sum();
        total as f64 / self.profiles.len() as f64
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. SchemaMigrator — Schema 迁移工具
// ═══════════════════════════════════════════════════════════════════════

/// 迁移类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOp {
    AddColumn,
    DropColumn,
    RenameColumn,
    AddIndex,
    DropIndex,
    RenameTable,
    ChangeType,
}

/// 迁移步骤
#[derive(Debug, Clone)]
pub struct MigrationStep {
    pub version: u64,
    pub description: String,
    pub op: MigrationOp,
    pub sql_up: String,
    pub sql_down: String,
    pub applied: bool,
}

/// Schema 迁移工具
pub struct SchemaMigrator {
    migrations: Vec<MigrationStep>,
    current_version: u64,
    applied_count: u64,
    rolled_back_count: u64,
}

impl SchemaMigrator {
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
            current_version: 0,
            applied_count: 0,
            rolled_back_count: 0,
        }
    }

    pub fn add_migration(
        &mut self,
        version: u64,
        desc: &str,
        op: MigrationOp,
        sql_up: &str,
        sql_down: &str,
    ) {
        self.migrations.push(MigrationStep {
            version,
            description: desc.to_string(),
            op,
            sql_up: sql_up.to_string(),
            sql_down: sql_down.to_string(),
            applied: false,
        });
        // Keep sorted by version
        self.migrations.sort_by_key(|m| m.version);
    }

    /// 应用到目标版本
    pub fn migrate_to(&mut self, target_version: u64) -> Vec<String> {
        let mut sqls = Vec::new();
        for m in &mut self.migrations {
            if m.version <= target_version && !m.applied {
                sqls.push(m.sql_up.clone());
                m.applied = true;
                self.current_version = m.version;
                self.applied_count += 1;
            }
        }
        sqls
    }

    /// 回滚到目标版本
    pub fn rollback_to(&mut self, target_version: u64) -> Vec<String> {
        let mut sqls = Vec::new();
        for m in self.migrations.iter_mut().rev() {
            if m.version > target_version && m.applied {
                sqls.push(m.sql_down.clone());
                m.applied = false;
                self.rolled_back_count += 1;
            }
        }
        self.current_version = target_version;
        sqls
    }

    pub fn current_version(&self) -> u64 {
        self.current_version
    }

    pub fn pending_count(&self) -> usize {
        self.migrations.iter().filter(|m| !m.applied).count()
    }

    pub fn applied_count(&self) -> u64 {
        self.applied_count
    }

    pub fn total_migrations(&self) -> usize {
        self.migrations.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. DataTransporter — 数据导入导出
// ═══════════════════════════════════════════════════════════════════════

/// 导出格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Parquet,
    SqlDump,
}

/// 导入导出任务
#[derive(Debug, Clone)]
pub struct TransportJob {
    pub job_id: u64,
    pub table_name: String,
    pub format: ExportFormat,
    pub is_export: bool,
    pub rows_processed: u64,
    pub bytes_processed: u64,
    pub completed: bool,
    pub error: Option<String>,
}

/// 数据导入导出器
pub struct DataTransporter {
    jobs: Vec<TransportJob>,
    next_job_id: u64,
    total_exported_rows: u64,
    total_imported_rows: u64,
}

impl DataTransporter {
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_job_id: 1,
            total_exported_rows: 0,
            total_imported_rows: 0,
        }
    }

    pub fn create_export(&mut self, table: &str, format: ExportFormat) -> u64 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(TransportJob {
            job_id: id,
            table_name: table.to_string(),
            format,
            is_export: true,
            rows_processed: 0,
            bytes_processed: 0,
            completed: false,
            error: None,
        });
        id
    }

    pub fn create_import(&mut self, table: &str, format: ExportFormat) -> u64 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(TransportJob {
            job_id: id,
            table_name: table.to_string(),
            format,
            is_export: false,
            rows_processed: 0,
            bytes_processed: 0,
            completed: false,
            error: None,
        });
        id
    }

    pub fn process_batch(&mut self, job_id: u64, rows: u64, bytes: u64) -> bool {
        if let Some(j) = self
            .jobs
            .iter_mut()
            .find(|j| j.job_id == job_id && !j.completed)
        {
            j.rows_processed += rows;
            j.bytes_processed += bytes;
            if j.is_export {
                self.total_exported_rows += rows;
            } else {
                self.total_imported_rows += rows;
            }
            true
        } else {
            false
        }
    }

    pub fn complete_job(&mut self, job_id: u64) -> bool {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.job_id == job_id) {
            j.completed = true;
            true
        } else {
            false
        }
    }

    pub fn job_status(&self, job_id: u64) -> Option<&TransportJob> {
        self.jobs.iter().find(|j| j.job_id == job_id)
    }

    pub fn active_jobs(&self) -> usize {
        self.jobs.iter().filter(|j| !j.completed).count()
    }

    pub fn total_exported_rows(&self) -> u64 {
        self.total_exported_rows
    }

    pub fn total_imported_rows(&self) -> u64 {
        self.total_imported_rows
    }

    /// 生成简单的 CSV 行
    pub fn format_csv_row(values: &[&str]) -> String {
        values
            .iter()
            .map(|v| {
                if v.contains(',') || v.contains('"') {
                    format!("\"{}\"", v.replace('"', "\"\""))
                } else {
                    v.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_explain_visualizer() {
        let mut vis = ExplainVisualizer::new();
        let scan = vis.add_node("SeqScan", Some("users"), 1000, 50.0, vec![]);
        let filter = vis.add_node("Filter", None, 100, 10.0, vec![scan]);
        let _proj = vis.add_node("Project", None, 100, 5.0, vec![filter]);

        assert_eq!(vis.node_count(), 3);
        assert!((vis.total_cost() - 65.0).abs() < 0.1);
        let tree = vis.render_tree();
        assert!(tree.contains("Project"));
        assert!(tree.contains("SeqScan"));
    }

    #[test]
    fn test_explain_bottleneck() {
        let mut vis = ExplainVisualizer::new();
        vis.add_node("Scan", Some("t1"), 10000, 100.0, vec![]);
        vis.add_node("Filter", None, 100, 5.0, vec![0]);
        let bottleneck = vis.bottleneck().unwrap();
        assert_eq!(bottleneck.operator, "Scan");
    }

    #[test]
    fn test_explain_set_actual() {
        let mut vis = ExplainVisualizer::new();
        let id = vis.add_node("IndexScan", Some("orders"), 500, 20.0, vec![]);
        vis.set_actual(id, 480, 5000);
        let tree = vis.render_tree();
        assert!(tree.contains("actual: 480 rows"));
    }

    #[test]
    fn test_query_profiler() {
        let mut profiler = QueryProfiler::new(100);
        profiler.record(QueryProfile {
            sql: "SELECT * FROM t".into(),
            parse_us: 10,
            optimize_us: 50,
            execute_us: 1000,
            total_us: 1060,
            rows_scanned: 10000,
            rows_returned: 100,
            buffer_hits: 900,
            buffer_misses: 100,
            io_reads: 10,
        });
        profiler.record(QueryProfile {
            sql: "INSERT INTO t VALUES (1)".into(),
            parse_us: 5,
            optimize_us: 10,
            execute_us: 50,
            total_us: 65,
            rows_scanned: 0,
            rows_returned: 0,
            buffer_hits: 10,
            buffer_misses: 1,
            io_reads: 1,
        });
        let slowest = profiler.slowest(1);
        assert_eq!(slowest[0].total_us, 1060);
        assert!(profiler.avg_latency_us() > 0.0);
    }

    #[test]
    fn test_query_profile_metrics() {
        let p = QueryProfile {
            sql: "q".into(),
            parse_us: 0,
            optimize_us: 0,
            execute_us: 0,
            total_us: 0,
            rows_scanned: 1000,
            rows_returned: 10,
            buffer_hits: 90,
            buffer_misses: 10,
            io_reads: 0,
        };
        assert!((p.scan_efficiency() - 0.01).abs() < 0.001);
        assert!((p.buffer_hit_rate() - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_schema_migrator() {
        let mut mig = SchemaMigrator::new();
        mig.add_migration(
            1,
            "add email",
            MigrationOp::AddColumn,
            "ALTER TABLE users ADD COLUMN email TEXT",
            "ALTER TABLE users DROP COLUMN email",
        );
        mig.add_migration(
            2,
            "add index",
            MigrationOp::AddIndex,
            "CREATE INDEX idx_email ON users(email)",
            "DROP INDEX idx_email",
        );

        let sqls = mig.migrate_to(2);
        assert_eq!(sqls.len(), 2);
        assert_eq!(mig.current_version(), 2);
        assert_eq!(mig.pending_count(), 0);

        let downs = mig.rollback_to(0);
        assert_eq!(downs.len(), 2);
        assert_eq!(mig.current_version(), 0);
    }

    #[test]
    fn test_schema_migrator_partial() {
        let mut mig = SchemaMigrator::new();
        mig.add_migration(1, "v1", MigrationOp::AddColumn, "UP 1", "DOWN 1");
        mig.add_migration(2, "v2", MigrationOp::AddColumn, "UP 2", "DOWN 2");
        mig.add_migration(3, "v3", MigrationOp::AddIndex, "UP 3", "DOWN 3");

        mig.migrate_to(2);
        assert_eq!(mig.current_version(), 2);
        assert_eq!(mig.pending_count(), 1);
    }

    #[test]
    fn test_data_transporter_export() {
        let mut dt = DataTransporter::new();
        let jid = dt.create_export("users", ExportFormat::Csv);
        dt.process_batch(jid, 1000, 50000);
        dt.process_batch(jid, 500, 25000);
        dt.complete_job(jid);

        let status = dt.job_status(jid).unwrap();
        assert!(status.completed);
        assert_eq!(status.rows_processed, 1500);
        assert_eq!(dt.total_exported_rows(), 1500);
    }

    #[test]
    fn test_data_transporter_import() {
        let mut dt = DataTransporter::new();
        let jid = dt.create_import("orders", ExportFormat::Json);
        dt.process_batch(jid, 2000, 100000);
        assert_eq!(dt.active_jobs(), 1);
        dt.complete_job(jid);
        assert_eq!(dt.active_jobs(), 0);
        assert_eq!(dt.total_imported_rows(), 2000);
    }

    #[test]
    fn test_csv_format() {
        let row = DataTransporter::format_csv_row(&["hello", "wor,ld", "fo\"o"]);
        assert!(row.contains("\"wor,ld\""));
        assert!(row.contains("\"fo\"\"o\""));
    }

    #[test]
    fn test_profiler_least_efficient() {
        let mut profiler = QueryProfiler::new(100);
        profiler.record(QueryProfile {
            sql: "q1".into(),
            parse_us: 0,
            optimize_us: 0,
            execute_us: 0,
            total_us: 100,
            rows_scanned: 10000,
            rows_returned: 1,
            buffer_hits: 0,
            buffer_misses: 0,
            io_reads: 0,
        });
        profiler.record(QueryProfile {
            sql: "q2".into(),
            parse_us: 0,
            optimize_us: 0,
            execute_us: 0,
            total_us: 50,
            rows_scanned: 100,
            rows_returned: 50,
            buffer_hits: 0,
            buffer_misses: 0,
            io_reads: 0,
        });
        let worst = profiler.least_efficient(1);
        assert_eq!(worst[0].sql, "q1"); // 1/10000 = 0.0001 efficiency
    }
}
