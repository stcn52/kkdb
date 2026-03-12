// ── src/vm/auth/security_adv.rs ──
// R20: 安全与审计强化 — 细粒度权限 / 数据脱敏 / 加密存储 / 合规审计日志

use std::collections::{HashMap, HashSet, VecDeque};

// ═══════════════════════════════════════════════════════════════════════
// 1. FineGrainedPermission — 列级/行级细粒度权限
// ═══════════════════════════════════════════════════════════════════════

/// 操作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermOp {
    Select,
    Insert,
    Update,
    Delete,
}

/// 行级安全策略
#[derive(Debug, Clone)]
pub struct RowPolicy {
    pub policy_name: String,
    pub table: String,
    pub predicate: String, // e.g. "department = CURRENT_USER_DEPT"
    pub for_ops: Vec<PermOp>,
}

/// 列级权限
#[derive(Debug, Clone)]
pub struct ColumnGrant {
    pub table: String,
    pub column: String,
    pub ops: HashSet<PermOp>,
}

/// 细粒度权限管理器
pub struct FineGrainedPermManager {
    /// role -> table -> column grants
    column_grants: HashMap<String, Vec<ColumnGrant>>,
    /// role -> row policies
    row_policies: HashMap<String, Vec<RowPolicy>>,
    check_count: u64,
}

impl FineGrainedPermManager {
    pub fn new() -> Self {
        Self {
            column_grants: HashMap::new(),
            row_policies: HashMap::new(),
            check_count: 0,
        }
    }

    pub fn grant_column(&mut self, role: &str, table: &str, column: &str, ops: Vec<PermOp>) {
        let grants = self.column_grants.entry(role.to_string()).or_default();
        grants.push(ColumnGrant {
            table: table.to_string(),
            column: column.to_string(),
            ops: ops.into_iter().collect(),
        });
    }

    pub fn add_row_policy(&mut self, role: &str, policy: RowPolicy) {
        self.row_policies.entry(role.to_string()).or_default().push(policy);
    }

    /// 检查列权限
    pub fn check_column(&mut self, role: &str, table: &str, column: &str, op: PermOp) -> bool {
        self.check_count += 1;
        let grants = match self.column_grants.get(role) {
            Some(g) => g,
            None => return false,
        };
        grants.iter().any(|g| g.table == table && g.column == column && g.ops.contains(&op))
    }

    /// 获得行级过滤条件
    pub fn row_filter(&self, role: &str, table: &str, op: PermOp) -> Option<String> {
        let policies = self.row_policies.get(role)?;
        for p in policies {
            if p.table == table && p.for_ops.contains(&op) {
                return Some(p.predicate.clone());
            }
        }
        None
    }

    pub fn check_count(&self) -> u64 {
        self.check_count
    }

    pub fn grant_count(&self) -> usize {
        self.column_grants.values().map(|v| v.len()).sum()
    }

    pub fn policy_count(&self) -> usize {
        self.row_policies.values().map(|v| v.len()).sum()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. DataMasker — 数据脱敏引擎
// ═══════════════════════════════════════════════════════════════════════

/// 脱敏规则
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskingRule {
    Full,                       // 完全遮蔽 → "***"
    Partial(usize, usize),     // 保留前N后M → "张*明"
    Hash,                       // 哈希替换
    Email,                      // username -> u***e@domain
    Phone,                      // 138****1234
    Custom(String),             // 自定义正则模式
}

/// 脱敏策略
#[derive(Debug, Clone)]
pub struct MaskingPolicy {
    pub table: String,
    pub column: String,
    pub rule: MaskingRule,
    pub exempt_roles: HashSet<String>,
}

/// 数据脱敏器
pub struct DataMasker {
    policies: Vec<MaskingPolicy>,
    masked_count: u64,
}

impl DataMasker {
    pub fn new() -> Self {
        Self {
            policies: Vec::new(),
            masked_count: 0,
        }
    }

    pub fn add_policy(&mut self, policy: MaskingPolicy) {
        self.policies.push(policy);
    }

    /// 对值执行脱敏
    pub fn mask_value(&mut self, table: &str, column: &str, role: &str, value: &str) -> String {
        let policy = match self.policies.iter().find(|p| p.table == table && p.column == column) {
            Some(p) => p,
            None => return value.to_string(),
        };

        if policy.exempt_roles.contains(role) {
            return value.to_string();
        }

        self.masked_count += 1;

        match &policy.rule {
            MaskingRule::Full => "***".to_string(),
            MaskingRule::Partial(prefix, suffix) => {
                let chars: Vec<char> = value.chars().collect();
                let len = chars.len();
                if *prefix + *suffix >= len {
                    return "*".repeat(len);
                }
                let mut result = String::new();
                for (i, c) in chars.iter().enumerate() {
                    if i < *prefix || i >= len - suffix {
                        result.push(*c);
                    } else {
                        result.push('*');
                    }
                }
                result
            }
            MaskingRule::Hash => {
                let hash = value.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
                format!("HASH_{:016x}", hash)
            }
            MaskingRule::Email => {
                if let Some(at_pos) = value.find('@') {
                    let user = &value[..at_pos];
                    let domain = &value[at_pos..];
                    if user.len() <= 2 {
                        format!("***{}", domain)
                    } else {
                        let first = &user[..1];
                        let last = &user[user.len()-1..];
                        format!("{}***{}{}", first, last, domain)
                    }
                } else {
                    "***".to_string()
                }
            }
            MaskingRule::Phone => {
                let chars: Vec<char> = value.chars().collect();
                if chars.len() >= 7 {
                    let prefix: String = chars[..3].iter().collect();
                    let suffix: String = chars[chars.len()-4..].iter().collect();
                    format!("{}****{}", prefix, suffix)
                } else {
                    "***".to_string()
                }
            }
            MaskingRule::Custom(pattern) => {
                format!("[MASKED:{}]", pattern)
            }
        }
    }

    pub fn policy_count(&self) -> usize {
        self.policies.len()
    }

    pub fn masked_count(&self) -> u64 {
        self.masked_count
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. EncryptedStorage — 存储加密管理
// ═══════════════════════════════════════════════════════════════════════

/// 加密算法
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionAlgo {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

/// 密钥元数据
#[derive(Debug, Clone)]
pub struct KeyMetadata {
    pub key_id: u64,
    pub algo: EncryptionAlgo,
    pub created_at_ms: u64,
    pub rotated_at_ms: u64,
    pub version: u32,
    pub active: bool,
}

/// 加密配置
#[derive(Debug, Clone)]
pub struct EncryptionConfig {
    pub table: String,
    pub columns: Vec<String>,
    pub key_id: u64,
    pub algo: EncryptionAlgo,
}

/// 加密存储管理器
pub struct EncryptedStorageManager {
    keys: HashMap<u64, KeyMetadata>,
    configs: HashMap<String, EncryptionConfig>,
    next_key_id: u64,
    encrypt_ops: u64,
    decrypt_ops: u64,
}

impl EncryptedStorageManager {
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
            configs: HashMap::new(),
            next_key_id: 1,
            encrypt_ops: 0,
            decrypt_ops: 0,
        }
    }

    pub fn create_key(&mut self, algo: EncryptionAlgo) -> u64 {
        let id = self.next_key_id;
        self.next_key_id += 1;
        self.keys.insert(id, KeyMetadata {
            key_id: id,
            algo,
            created_at_ms: 0,
            rotated_at_ms: 0,
            version: 1,
            active: true,
        });
        id
    }

    pub fn rotate_key(&mut self, key_id: u64) -> bool {
        if let Some(meta) = self.keys.get_mut(&key_id) {
            meta.version += 1;
            meta.rotated_at_ms = 0; // in real impl: current timestamp
            true
        } else {
            false
        }
    }

    pub fn deactivate_key(&mut self, key_id: u64) -> bool {
        if let Some(meta) = self.keys.get_mut(&key_id) {
            meta.active = false;
            true
        } else {
            false
        }
    }

    pub fn configure_table(&mut self, table: &str, columns: Vec<String>, key_id: u64, algo: EncryptionAlgo) -> bool {
        if !self.keys.contains_key(&key_id) {
            return false;
        }
        self.configs.insert(table.to_string(), EncryptionConfig {
            table: table.to_string(),
            columns,
            key_id,
            algo,
        });
        true
    }

    /// 模拟加密
    pub fn encrypt(&mut self, table: &str, column: &str, data: &[u8]) -> Option<Vec<u8>> {
        let config = self.configs.get(table)?;
        if !config.columns.iter().any(|c| c == column) {
            return None;
        }
        let key = self.keys.get(&config.key_id)?;
        if !key.active {
            return None;
        }
        self.encrypt_ops += 1;
        // Simulated: XOR with key_id bytes
        let encrypted: Vec<u8> = data.iter().map(|b| b ^ (key.key_id as u8)).collect();
        Some(encrypted)
    }

    /// 模拟解密
    pub fn decrypt(&mut self, table: &str, column: &str, data: &[u8]) -> Option<Vec<u8>> {
        let config = self.configs.get(table)?;
        if !config.columns.iter().any(|c| c == column) {
            return None;
        }
        let key = self.keys.get(&config.key_id)?;
        self.decrypt_ops += 1;
        // XOR is self-inverse
        let decrypted: Vec<u8> = data.iter().map(|b| b ^ (key.key_id as u8)).collect();
        Some(decrypted)
    }

    pub fn active_key_count(&self) -> usize {
        self.keys.values().filter(|k| k.active).count()
    }

    pub fn encrypt_ops(&self) -> u64 {
        self.encrypt_ops
    }

    pub fn decrypt_ops(&self) -> u64 {
        self.decrypt_ops
    }

    pub fn configured_table_count(&self) -> usize {
        self.configs.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. ComplianceAuditLog — 合规审计日志
// ═══════════════════════════════════════════════════════════════════════

/// 审计事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEventType {
    Login,
    Logout,
    QueryExecute,
    DdlChange,
    PermissionChange,
    DataAccess,
    DataExport,
    SchemaChange,
    ConfigChange,
    SecurityViolation,
}

/// 合规标准
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComplianceStandard {
    Gdpr,
    Hipaa,
    Sox,
    Pci,
    Custom,
}

/// 审计日志条目
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub event_id: u64,
    pub event_type: AuditEventType,
    pub user: String,
    pub resource: String,
    pub action: String,
    pub result: AuditResult,
    pub timestamp_ms: u64,
    pub details: HashMap<String, String>,
    pub standards: Vec<ComplianceStandard>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditResult {
    Success,
    Denied,
    Error,
}

/// 合规审计日志管理器
pub struct ComplianceAuditLogger {
    entries: VecDeque<AuditEntry>,
    max_entries: usize,
    next_event_id: u64,
    required_standards: HashSet<ComplianceStandard>,
    violation_count: u64,
}

impl ComplianceAuditLogger {
    pub fn new(max_entries: usize, standards: Vec<ComplianceStandard>) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
            next_event_id: 1,
            required_standards: standards.into_iter().collect(),
            violation_count: 0,
        }
    }

    pub fn log(&mut self, event_type: AuditEventType, user: &str, resource: &str, action: &str, result: AuditResult) -> u64 {
        let id = self.next_event_id;
        self.next_event_id += 1;

        if result == AuditResult::Denied {
            self.violation_count += 1;
        }

        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }

        let standards: Vec<ComplianceStandard> = self.required_standards.iter().copied().collect();

        self.entries.push_back(AuditEntry {
            event_id: id,
            event_type,
            user: user.to_string(),
            resource: resource.to_string(),
            action: action.to_string(),
            result,
            timestamp_ms: 0,
            details: HashMap::new(),
            standards,
        });
        id
    }

    pub fn log_with_details(&mut self, event_type: AuditEventType, user: &str, resource: &str, action: &str, result: AuditResult, details: HashMap<String, String>) -> u64 {
        let id = self.log(event_type, user, resource, action, result);
        if let Some(entry) = self.entries.back_mut() {
            entry.details = details;
        }
        id
    }

    /// 查询过滤
    pub fn query(&self, user: Option<&str>, event_type: Option<AuditEventType>) -> Vec<&AuditEntry> {
        self.entries
            .iter()
            .filter(|e| {
                user.map_or(true, |u| e.user == u)
                    && event_type.map_or(true, |t| e.event_type == t)
            })
            .collect()
    }

    /// 合规报告
    pub fn compliance_report(&self) -> HashMap<String, usize> {
        let mut report = HashMap::new();
        report.insert("total_events".into(), self.entries.len());
        report.insert("violations".into(), self.violation_count as usize);

        let mut by_type: HashMap<String, usize> = HashMap::new();
        for entry in &self.entries {
            *by_type.entry(format!("{:?}", entry.event_type)).or_insert(0) += 1;
        }
        for (k, v) in by_type {
            report.insert(k, v);
        }
        report
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn violation_count(&self) -> u64 {
        self.violation_count
    }

    pub fn has_standard(&self, std: ComplianceStandard) -> bool {
        self.required_standards.contains(&std)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fine_grained_column_perm() {
        let mut mgr = FineGrainedPermManager::new();
        mgr.grant_column("analyst", "users", "name", vec![PermOp::Select]);
        mgr.grant_column("analyst", "users", "email", vec![PermOp::Select]);
        assert!(mgr.check_column("analyst", "users", "name", PermOp::Select));
        assert!(!mgr.check_column("analyst", "users", "salary", PermOp::Select));
        assert!(!mgr.check_column("analyst", "users", "name", PermOp::Update));
        assert_eq!(mgr.grant_count(), 2);
    }

    #[test]
    fn test_row_policy() {
        let mut mgr = FineGrainedPermManager::new();
        mgr.add_row_policy("sales_rep", RowPolicy {
            policy_name: "sales_only".into(),
            table: "orders".into(),
            predicate: "region = CURRENT_USER_REGION".into(),
            for_ops: vec![PermOp::Select, PermOp::Update],
        });
        let filter = mgr.row_filter("sales_rep", "orders", PermOp::Select);
        assert_eq!(filter, Some("region = CURRENT_USER_REGION".to_string()));
        assert_eq!(mgr.row_filter("sales_rep", "orders", PermOp::Delete), None);
    }

    #[test]
    fn test_data_masker_full() {
        let mut masker = DataMasker::new();
        masker.add_policy(MaskingPolicy {
            table: "users".into(),
            column: "ssn".into(),
            rule: MaskingRule::Full,
            exempt_roles: HashSet::new(),
        });
        let masked = masker.mask_value("users", "ssn", "viewer", "123-45-6789");
        assert_eq!(masked, "***");
        assert_eq!(masker.masked_count(), 1);
    }

    #[test]
    fn test_data_masker_partial() {
        let mut masker = DataMasker::new();
        masker.add_policy(MaskingPolicy {
            table: "users".into(),
            column: "name".into(),
            rule: MaskingRule::Partial(1, 1),
            exempt_roles: HashSet::new(),
        });
        let masked = masker.mask_value("users", "name", "viewer", "Alice");
        assert_eq!(masked, "A***e");
    }

    #[test]
    fn test_data_masker_email() {
        let mut masker = DataMasker::new();
        masker.add_policy(MaskingPolicy {
            table: "users".into(),
            column: "email".into(),
            rule: MaskingRule::Email,
            exempt_roles: ["admin".to_string()].into_iter().collect(),
        });
        let masked = masker.mask_value("users", "email", "viewer", "alice@example.com");
        assert_eq!(masked, "a***e@example.com");
        // Admin is exempt
        let unmasked = masker.mask_value("users", "email", "admin", "alice@example.com");
        assert_eq!(unmasked, "alice@example.com");
    }

    #[test]
    fn test_data_masker_phone() {
        let mut masker = DataMasker::new();
        masker.add_policy(MaskingPolicy {
            table: "contacts".into(),
            column: "phone".into(),
            rule: MaskingRule::Phone,
            exempt_roles: HashSet::new(),
        });
        let masked = masker.mask_value("contacts", "phone", "user", "13812345678");
        assert_eq!(masked, "138****5678");
    }

    #[test]
    fn test_encrypted_storage_roundtrip() {
        let mut mgr = EncryptedStorageManager::new();
        let key_id = mgr.create_key(EncryptionAlgo::Aes256Gcm);
        mgr.configure_table("secrets", vec!["token".into()], key_id, EncryptionAlgo::Aes256Gcm);

        let data = b"hello_secret";
        let encrypted = mgr.encrypt("secrets", "token", data).unwrap();
        assert_ne!(encrypted, data);
        let decrypted = mgr.decrypt("secrets", "token", &encrypted).unwrap();
        assert_eq!(decrypted, data);
        assert_eq!(mgr.encrypt_ops(), 1);
        assert_eq!(mgr.decrypt_ops(), 1);
    }

    #[test]
    fn test_key_rotation() {
        let mut mgr = EncryptedStorageManager::new();
        let kid = mgr.create_key(EncryptionAlgo::Aes128Gcm);
        assert!(mgr.rotate_key(kid));
        assert_eq!(mgr.active_key_count(), 1);
        assert!(mgr.deactivate_key(kid));
        assert_eq!(mgr.active_key_count(), 0);
    }

    #[test]
    fn test_encrypt_inactive_key() {
        let mut mgr = EncryptedStorageManager::new();
        let kid = mgr.create_key(EncryptionAlgo::ChaCha20Poly1305);
        mgr.configure_table("t", vec!["c".into()], kid, EncryptionAlgo::ChaCha20Poly1305);
        mgr.deactivate_key(kid);
        assert!(mgr.encrypt("t", "c", b"data").is_none());
    }

    #[test]
    fn test_compliance_audit_log() {
        let mut logger = ComplianceAuditLogger::new(1000, vec![ComplianceStandard::Gdpr, ComplianceStandard::Sox]);
        logger.log(AuditEventType::Login, "admin", "system", "LOGIN", AuditResult::Success);
        logger.log(AuditEventType::DataAccess, "user1", "users.email", "SELECT", AuditResult::Success);
        logger.log(AuditEventType::SecurityViolation, "attacker", "admin_panel", "ACCESS", AuditResult::Denied);

        assert_eq!(logger.entry_count(), 3);
        assert_eq!(logger.violation_count(), 1);

        let user_events = logger.query(Some("user1"), None);
        assert_eq!(user_events.len(), 1);

        let violations = logger.query(None, Some(AuditEventType::SecurityViolation));
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn test_audit_compliance_report() {
        let mut logger = ComplianceAuditLogger::new(100, vec![ComplianceStandard::Hipaa]);
        logger.log(AuditEventType::DataAccess, "doc", "patient_records", "READ", AuditResult::Success);
        logger.log(AuditEventType::DataExport, "doc", "patient_records", "EXPORT", AuditResult::Denied);

        let report = logger.compliance_report();
        assert_eq!(*report.get("total_events").unwrap(), 2);
        assert_eq!(*report.get("violations").unwrap(), 1);
        assert!(logger.has_standard(ComplianceStandard::Hipaa));
    }

    #[test]
    fn test_audit_max_entries() {
        let mut logger = ComplianceAuditLogger::new(2, vec![]);
        logger.log(AuditEventType::Login, "a", "", "", AuditResult::Success);
        logger.log(AuditEventType::Login, "b", "", "", AuditResult::Success);
        logger.log(AuditEventType::Login, "c", "", "", AuditResult::Success);
        assert_eq!(logger.entry_count(), 2);
        // First entry evicted
        let all = logger.query(Some("a"), None);
        assert!(all.is_empty());
    }
}
