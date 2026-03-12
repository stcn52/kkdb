// R16 – Security & compliance: column-level encryption, audit log archival,
//       dynamic data masking, SSL/TLS simulation, password policy engine.
//
// Provides:
//   - `ColumnEncryption`: per-column encryption key management + encrypt/decrypt
//   - `AuditArchiver`: rotate & archive audit logs by size/age
//   - `DataMasker`: dynamic masking rules (email, phone, SSN, etc.)
//   - `TlsConfig`: connection TLS level & cert tracking
//   - `PasswordPolicy`: configurable password strength rules

use std::collections::HashMap;

// ── Column-Level Encryption ───────────────────────────────────────────

/// Encryption algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EncryptionAlgo {
    Aes128,
    Aes256,
    ChaCha20,
}

/// Per-column encryption config.
#[derive(Debug, Clone)]
pub struct ColumnEncryptionDef {
    pub table: String,
    pub column: String,
    pub algo: EncryptionAlgo,
    pub key_id: String,
}

/// Manages column-level encryption keys and operations.
pub struct ColumnEncryption {
    defs: Vec<ColumnEncryptionDef>,
    /// key_id → key bytes (simulated).
    keys: HashMap<String, Vec<u8>>,
}

impl ColumnEncryption {
    pub fn new() -> Self {
        Self {
            defs: Vec::new(),
            keys: HashMap::new(),
        }
    }

    /// Register an encryption key.
    pub fn add_key(&mut self, key_id: &str, key_bytes: Vec<u8>) {
        self.keys.insert(key_id.to_string(), key_bytes);
    }

    /// Register a column for encryption.
    pub fn encrypt_column(&mut self, table: &str, column: &str, algo: EncryptionAlgo, key_id: &str) -> bool {
        if !self.keys.contains_key(key_id) {
            return false;
        }
        self.defs.push(ColumnEncryptionDef {
            table: table.to_string(),
            column: column.to_string(),
            algo,
            key_id: key_id.to_string(),
        });
        true
    }

    /// Check if a column is encrypted.
    pub fn is_encrypted(&self, table: &str, column: &str) -> bool {
        self.defs.iter().any(|d| d.table == table && d.column == column)
    }

    /// Simulate encryption (XOR with key for demo purposes).
    pub fn encrypt(&self, table: &str, column: &str, data: &[u8]) -> Option<Vec<u8>> {
        let def = self.defs.iter().find(|d| d.table == table && d.column == column)?;
        let key = self.keys.get(&def.key_id)?;
        let encrypted: Vec<u8> = data.iter().enumerate()
            .map(|(i, b)| b ^ key[i % key.len()])
            .collect();
        Some(encrypted)
    }

    /// Simulate decryption (XOR is symmetric).
    pub fn decrypt(&self, table: &str, column: &str, data: &[u8]) -> Option<Vec<u8>> {
        // XOR encryption is symmetric
        self.encrypt(table, column, data)
    }

    pub fn encrypted_column_count(&self) -> usize {
        self.defs.len()
    }
}

// ── Audit Log Archiver ────────────────────────────────────────────────

/// An archived audit log segment.
#[derive(Debug, Clone)]
pub struct AuditArchive {
    pub archive_id: u64,
    pub entry_count: usize,
    pub start_time: u64,
    pub end_time: u64,
    pub byte_size: usize,
    pub is_compressed: bool,
}

/// Manages audit log rotation and archival.
pub struct AuditArchiver {
    /// Current buffer of audit entries.
    current_entries: usize,
    current_bytes: usize,
    current_start: u64,
    /// Rotation thresholds.
    max_entries: usize,
    max_bytes: usize,
    /// Archived segments.
    archives: Vec<AuditArchive>,
    next_id: u64,
    /// Retention period (simulated tick count).
    retention_period: u64,
}

impl AuditArchiver {
    pub fn new(max_entries: usize, max_bytes: usize, retention_period: u64) -> Self {
        Self {
            current_entries: 0,
            current_bytes: 0,
            current_start: 0,
            max_entries,
            max_bytes,
            archives: Vec::new(),
            next_id: 1,
            retention_period,
        }
    }

    /// Add an audit entry. Returns true if rotation was triggered.
    pub fn add_entry(&mut self, byte_size: usize, timestamp: u64) -> bool {
        if self.current_entries == 0 {
            self.current_start = timestamp;
        }
        self.current_entries += 1;
        self.current_bytes += byte_size;

        if self.current_entries >= self.max_entries || self.current_bytes >= self.max_bytes {
            self.rotate(timestamp);
            true
        } else {
            false
        }
    }

    /// Force rotation of current entries into an archive.
    pub fn rotate(&mut self, timestamp: u64) {
        if self.current_entries == 0 {
            return;
        }
        self.archives.push(AuditArchive {
            archive_id: self.next_id,
            entry_count: self.current_entries,
            start_time: self.current_start,
            end_time: timestamp,
            byte_size: self.current_bytes,
            is_compressed: true,
        });
        self.next_id += 1;
        self.current_entries = 0;
        self.current_bytes = 0;
    }

    /// Purge archives older than retention period.
    pub fn purge_old(&mut self, current_time: u64) -> usize {
        let before = self.archives.len();
        self.archives.retain(|a| current_time - a.end_time < self.retention_period);
        before - self.archives.len()
    }

    pub fn archive_count(&self) -> usize {
        self.archives.len()
    }

    pub fn current_entries(&self) -> usize {
        self.current_entries
    }

    pub fn total_archived_bytes(&self) -> usize {
        self.archives.iter().map(|a| a.byte_size).sum()
    }
}

// ── Dynamic Data Masking ──────────────────────────────────────────────

/// Masking strategies.
#[derive(Debug, Clone, PartialEq)]
pub enum MaskStrategy {
    /// Replace with fixed string.
    Full(String),
    /// Show first N, last M characters.
    Partial { show_first: usize, show_last: usize, mask_char: char },
    /// Email masking: show first char + domain.
    Email,
    /// Phone: show last 4 digits.
    Phone,
    /// Hash the value.
    Hash,
}

/// A masking rule.
#[derive(Debug, Clone)]
pub struct MaskRule {
    pub table: String,
    pub column: String,
    pub strategy: MaskStrategy,
    /// Roles that can see unmasked data.
    pub exempt_roles: Vec<String>,
}

/// Dynamic data masking engine.
pub struct DataMasker {
    rules: Vec<MaskRule>,
}

impl DataMasker {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Add a masking rule.
    pub fn add_rule(&mut self, table: &str, column: &str, strategy: MaskStrategy, exempt_roles: Vec<String>) {
        self.rules.push(MaskRule {
            table: table.to_string(),
            column: column.to_string(),
            strategy,
            exempt_roles,
        });
    }

    /// Apply masking to a value.
    pub fn mask(&self, table: &str, column: &str, value: &str, user_role: &str) -> String {
        let rule = match self.rules.iter().find(|r| r.table == table && r.column == column) {
            Some(r) => r,
            None => return value.to_string(), // no rule → pass through
        };

        // Check exemption
        if rule.exempt_roles.iter().any(|r| r == user_role) {
            return value.to_string();
        }

        match &rule.strategy {
            MaskStrategy::Full(replacement) => replacement.clone(),
            MaskStrategy::Partial { show_first, show_last, mask_char } => {
                let chars: Vec<char> = value.chars().collect();
                let len = chars.len();
                if len <= show_first + show_last {
                    return std::iter::repeat(*mask_char).take(len).collect();
                }
                let mut result = String::new();
                for (i, c) in chars.iter().enumerate() {
                    if i < *show_first || i >= len - show_last {
                        result.push(*c);
                    } else {
                        result.push(*mask_char);
                    }
                }
                result
            }
            MaskStrategy::Email => {
                if let Some(at_pos) = value.find('@') {
                    let first_char = value.chars().next().unwrap_or('*');
                    format!("{}***{}", first_char, &value[at_pos..])
                } else {
                    "***".to_string()
                }
            }
            MaskStrategy::Phone => {
                let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
                if digits.len() >= 4 {
                    format!("***{}", &digits[digits.len()-4..])
                } else {
                    "***".to_string()
                }
            }
            MaskStrategy::Hash => {
                // Simple hash for demo
                let mut h: u64 = 0;
                for b in value.bytes() {
                    h = h.wrapping_mul(31).wrapping_add(b as u64);
                }
                format!("{:016x}", h)
            }
        }
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Check if a column has masking.
    pub fn is_masked(&self, table: &str, column: &str) -> bool {
        self.rules.iter().any(|r| r.table == table && r.column == column)
    }
}

// ── TLS Configuration ─────────────────────────────────────────────────

/// TLS requirement level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TlsLevel {
    Disabled,
    Preferred,
    Required,
    VerifyCA,
    VerifyFull,
}

/// TLS/SSL connection configuration.
pub struct TlsConfig {
    pub level: TlsLevel,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub ca_path: Option<String>,
    connections_tls: u64,
    connections_plain: u64,
}

impl TlsConfig {
    pub fn new(level: TlsLevel) -> Self {
        Self {
            level,
            cert_path: None,
            key_path: None,
            ca_path: None,
            connections_tls: 0,
            connections_plain: 0,
        }
    }

    pub fn with_cert(mut self, cert: &str, key: &str) -> Self {
        self.cert_path = Some(cert.to_string());
        self.key_path = Some(key.to_string());
        self
    }

    pub fn with_ca(mut self, ca: &str) -> Self {
        self.ca_path = Some(ca.to_string());
        self
    }

    /// Check if a plaintext connection should be allowed.
    pub fn allows_plain(&self) -> bool {
        matches!(self.level, TlsLevel::Disabled | TlsLevel::Preferred)
    }

    /// Record a connection.
    pub fn record_connection(&mut self, is_tls: bool) {
        if is_tls {
            self.connections_tls += 1;
        } else {
            self.connections_plain += 1;
        }
    }

    pub fn tls_connections(&self) -> u64 {
        self.connections_tls
    }

    pub fn plain_connections(&self) -> u64 {
        self.connections_plain
    }

    pub fn tls_ratio(&self) -> f64 {
        let total = self.connections_tls + self.connections_plain;
        if total == 0 { return 0.0; }
        self.connections_tls as f64 / total as f64
    }
}

// ── Password Policy Engine ───────────────────────────────────────────

/// Password policy rules.
#[derive(Debug, Clone)]
pub struct PasswordPolicy {
    pub min_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_special: bool,
    pub max_age_days: u32,
    pub history_count: usize, // prevent reuse of last N passwords
}

impl PasswordPolicy {
    pub fn new() -> Self {
        Self {
            min_length: 8,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: false,
            max_age_days: 90,
            history_count: 3,
        }
    }

    pub fn strict() -> Self {
        Self {
            min_length: 12,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
            max_age_days: 30,
            history_count: 5,
        }
    }

    /// Validate a password against the policy.
    pub fn validate(&self, password: &str) -> Vec<String> {
        let mut issues = Vec::new();
        if password.len() < self.min_length {
            issues.push(format!("minimum length is {}", self.min_length));
        }
        if self.require_uppercase && !password.chars().any(|c| c.is_uppercase()) {
            issues.push("must contain uppercase letter".to_string());
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_lowercase()) {
            issues.push("must contain lowercase letter".to_string());
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            issues.push("must contain digit".to_string());
        }
        if self.require_special && !password.chars().any(|c| !c.is_alphanumeric()) {
            issues.push("must contain special character".to_string());
        }
        issues
    }

    /// Check if a password is strong enough.
    pub fn is_valid(&self, password: &str) -> bool {
        self.validate(password).is_empty()
    }

    /// Calculate password strength score (0-100).
    pub fn strength_score(password: &str) -> u32 {
        let mut score = 0u32;
        let len = password.len();
        score += (len.min(20) * 3) as u32;
        if password.chars().any(|c| c.is_uppercase()) { score += 10; }
        if password.chars().any(|c| c.is_lowercase()) { score += 10; }
        if password.chars().any(|c| c.is_ascii_digit()) { score += 10; }
        if password.chars().any(|c| !c.is_alphanumeric()) { score += 15; }
        let unique: std::collections::HashSet<char> = password.chars().collect();
        score += (unique.len().min(10) * 2) as u32;
        score.min(100)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_encryption_roundtrip() {
        let mut ce = ColumnEncryption::new();
        ce.add_key("key1", vec![0xAB, 0xCD, 0xEF, 0x12]);
        assert!(ce.encrypt_column("users", "ssn", EncryptionAlgo::Aes256, "key1"));
        assert!(ce.is_encrypted("users", "ssn"));

        let data = b"123-45-6789";
        let encrypted = ce.encrypt("users", "ssn", data).unwrap();
        assert_ne!(&encrypted, data);
        let decrypted = ce.decrypt("users", "ssn", &encrypted).unwrap();
        assert_eq!(&decrypted, data);
    }

    #[test]
    fn column_encryption_missing_key() {
        let mut ce = ColumnEncryption::new();
        assert!(!ce.encrypt_column("t", "c", EncryptionAlgo::Aes128, "nokey"));
    }

    #[test]
    fn audit_archiver_rotation() {
        let mut arch = AuditArchiver::new(5, 10000, 1000);
        for i in 0..4 {
            assert!(!arch.add_entry(100, i));
        }
        assert!(arch.add_entry(100, 5)); // 5th entry → rotation
        assert_eq!(arch.archive_count(), 1);
        assert_eq!(arch.current_entries(), 0);
    }

    #[test]
    fn audit_archiver_purge() {
        let mut arch = AuditArchiver::new(2, 10000, 100);
        arch.add_entry(50, 10);
        arch.add_entry(50, 20); // rotation
        arch.add_entry(50, 110);
        arch.add_entry(50, 120); // rotation
        assert_eq!(arch.archive_count(), 2);
        let purged = arch.purge_old(200);
        assert_eq!(purged, 1); // first archive (end_time=20) is >100 old
    }

    #[test]
    fn data_masker_email() {
        let mut dm = DataMasker::new();
        dm.add_rule("users", "email", MaskStrategy::Email, vec!["admin".to_string()]);
        let masked = dm.mask("users", "email", "john@example.com", "viewer");
        assert_eq!(masked, "j***@example.com");
        let unmasked = dm.mask("users", "email", "john@example.com", "admin");
        assert_eq!(unmasked, "john@example.com"); // exempted
    }

    #[test]
    fn data_masker_partial() {
        let mut dm = DataMasker::new();
        dm.add_rule("users", "phone", MaskStrategy::Partial {
            show_first: 2, show_last: 2, mask_char: '*'
        }, Vec::new());
        let masked = dm.mask("users", "phone", "1234567890", "any");
        assert_eq!(masked, "12******90");
    }

    #[test]
    fn data_masker_hash() {
        let mut dm = DataMasker::new();
        dm.add_rule("t", "c", MaskStrategy::Hash, Vec::new());
        let result = dm.mask("t", "c", "secret", "user");
        assert_eq!(result.len(), 16);
        assert!(dm.is_masked("t", "c"));
    }

    #[test]
    fn tls_config_basic() {
        let mut tls = TlsConfig::new(TlsLevel::Required);
        assert!(!tls.allows_plain());
        tls.record_connection(true);
        tls.record_connection(true);
        tls.record_connection(false);
        assert_eq!(tls.tls_connections(), 2);
        assert!((tls.tls_ratio() - 0.6666).abs() < 0.01);
    }

    #[test]
    fn tls_config_preferred() {
        let tls = TlsConfig::new(TlsLevel::Preferred);
        assert!(tls.allows_plain());
    }

    #[test]
    fn password_policy_validation() {
        let policy = PasswordPolicy::new();
        assert!(policy.is_valid("Abcdef1g"));
        assert!(!policy.is_valid("short"));
        let issues = policy.validate("abc");
        assert!(issues.iter().any(|i| i.contains("minimum length")));
        assert!(issues.iter().any(|i| i.contains("uppercase")));
        assert!(issues.iter().any(|i| i.contains("digit")));
    }

    #[test]
    fn password_policy_strict() {
        let policy = PasswordPolicy::strict();
        assert!(!policy.is_valid("Abcdef1g")); // no special char, short
        assert!(policy.is_valid("Abcdef1g!#@x"));
    }

    #[test]
    fn password_strength_score() {
        let weak = PasswordPolicy::strength_score("abc");
        let strong = PasswordPolicy::strength_score("MyP@ssw0rd!2024");
        assert!(strong > weak);
        assert!(strong > 50);
    }
}
