// R12 – Role-Based Access Control (RBAC) + Audit persistence.
//
// Provides:
//   - `Privilege`: individual permissions (SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, ALTER, GRANT, ALL)
//   - `Role`: named set of privileges
//   - `RbacManager`: manages users, roles, and privilege checks
//   - `AuditPersistence`: buffered writer for audit log entries to disk

use std::collections::{HashMap, HashSet};
use std::fmt;

// ── Privileges ────────────────────────────────────────────────────────

/// Individual SQL privilege.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Drop,
    Alter,
    Grant,
    Execute,
    All,
}

impl fmt::Display for Privilege {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Select => write!(f, "SELECT"),
            Self::Insert => write!(f, "INSERT"),
            Self::Update => write!(f, "UPDATE"),
            Self::Delete => write!(f, "DELETE"),
            Self::Create => write!(f, "CREATE"),
            Self::Drop => write!(f, "DROP"),
            Self::Alter => write!(f, "ALTER"),
            Self::Grant => write!(f, "GRANT"),
            Self::Execute => write!(f, "EXECUTE"),
            Self::All => write!(f, "ALL"),
        }
    }
}

impl Privilege {
    /// Parse a privilege from a string.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SELECT" => Some(Self::Select),
            "INSERT" => Some(Self::Insert),
            "UPDATE" => Some(Self::Update),
            "DELETE" => Some(Self::Delete),
            "CREATE" => Some(Self::Create),
            "DROP" => Some(Self::Drop),
            "ALTER" => Some(Self::Alter),
            "GRANT" => Some(Self::Grant),
            "EXECUTE" => Some(Self::Execute),
            "ALL" => Some(Self::All),
            _ => None,
        }
    }

    /// All concrete privileges (excluding ALL).
    pub fn all_concrete() -> &'static [Privilege] {
        &[
            Self::Select,
            Self::Insert,
            Self::Update,
            Self::Delete,
            Self::Create,
            Self::Drop,
            Self::Alter,
            Self::Grant,
            Self::Execute,
        ]
    }
}

// ── Role ──────────────────────────────────────────────────────────────

/// A named role with a set of privileges, optionally scoped to specific tables.
#[derive(Debug, Clone)]
pub struct Role {
    pub name: String,
    /// Global privileges (apply to all tables).
    pub global_privileges: HashSet<Privilege>,
    /// Table-specific privileges: table_name → set of privileges.
    pub table_privileges: HashMap<String, HashSet<Privilege>>,
}

impl Role {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            global_privileges: HashSet::new(),
            table_privileges: HashMap::new(),
        }
    }

    /// Grant a global privilege.
    pub fn grant_global(&mut self, priv_: Privilege) {
        if priv_ == Privilege::All {
            for p in Privilege::all_concrete() {
                self.global_privileges.insert(*p);
            }
        } else {
            self.global_privileges.insert(priv_);
        }
    }

    /// Grant a table-specific privilege.
    pub fn grant_table(&mut self, table: &str, priv_: Privilege) {
        let entry = self.table_privileges.entry(table.to_string()).or_default();
        if priv_ == Privilege::All {
            for p in Privilege::all_concrete() {
                entry.insert(*p);
            }
        } else {
            entry.insert(priv_);
        }
    }

    /// Revoke a global privilege.
    pub fn revoke_global(&mut self, priv_: Privilege) {
        if priv_ == Privilege::All {
            self.global_privileges.clear();
        } else {
            self.global_privileges.remove(&priv_);
        }
    }

    /// Revoke a table-specific privilege.
    pub fn revoke_table(&mut self, table: &str, priv_: Privilege) {
        if let Some(entry) = self.table_privileges.get_mut(table) {
            if priv_ == Privilege::All {
                entry.clear();
            } else {
                entry.remove(&priv_);
            }
        }
    }

    /// Check if this role has a specific privilege on a table.
    pub fn has_privilege(&self, table: &str, priv_: Privilege) -> bool {
        // Global privileges apply everywhere
        if self.global_privileges.contains(&priv_)
            || self.global_privileges.contains(&Privilege::All)
        {
            return true;
        }
        // Table-specific
        if let Some(tbl_privs) = self.table_privileges.get(table) {
            if tbl_privs.contains(&priv_) || tbl_privs.contains(&Privilege::All) {
                return true;
            }
        }
        false
    }

    /// List all effective privileges for a table.
    pub fn effective_privileges(&self, table: &str) -> HashSet<Privilege> {
        let mut result = self.global_privileges.clone();
        if let Some(tbl_privs) = self.table_privileges.get(table) {
            result.extend(tbl_privs);
        }
        result
    }
}

// ── RBAC Manager ──────────────────────────────────────────────────────

/// User record in the RBAC system.
#[derive(Debug, Clone)]
pub struct UserRecord {
    pub username: String,
    /// Roles assigned to this user.
    pub roles: HashSet<String>,
    /// Direct privileges (not through roles).
    pub direct_privileges: HashSet<Privilege>,
    /// Table-specific direct privileges.
    pub direct_table_privileges: HashMap<String, HashSet<Privilege>>,
    /// Is this a superuser (bypass all checks)?
    pub is_superuser: bool,
}

impl UserRecord {
    pub fn new(username: &str) -> Self {
        Self {
            username: username.to_string(),
            roles: HashSet::new(),
            direct_privileges: HashSet::new(),
            direct_table_privileges: HashMap::new(),
            is_superuser: false,
        }
    }
}

/// RBAC manager — manages users, roles, and authorization checks.
pub struct RbacManager {
    users: HashMap<String, UserRecord>,
    roles: HashMap<String, Role>,
}

impl RbacManager {
    pub fn new() -> Self {
        Self {
            users: HashMap::new(),
            roles: HashMap::new(),
        }
    }

    // ── Role management ───────────────────

    pub fn create_role(&mut self, name: &str) -> bool {
        if self.roles.contains_key(name) {
            return false;
        }
        self.roles.insert(name.to_string(), Role::new(name));
        true
    }

    pub fn drop_role(&mut self, name: &str) -> bool {
        if self.roles.remove(name).is_some() {
            // Remove role from all users
            for user in self.users.values_mut() {
                user.roles.remove(name);
            }
            true
        } else {
            false
        }
    }

    pub fn get_role(&self, name: &str) -> Option<&Role> {
        self.roles.get(name)
    }

    pub fn get_role_mut(&mut self, name: &str) -> Option<&mut Role> {
        self.roles.get_mut(name)
    }

    pub fn role_count(&self) -> usize {
        self.roles.len()
    }

    // ── User management ───────────────────

    pub fn create_user(&mut self, username: &str) -> bool {
        if self.users.contains_key(username) {
            return false;
        }
        self.users
            .insert(username.to_string(), UserRecord::new(username));
        true
    }

    pub fn drop_user(&mut self, username: &str) -> bool {
        self.users.remove(username).is_some()
    }

    pub fn get_user(&self, username: &str) -> Option<&UserRecord> {
        self.users.get(username)
    }

    pub fn user_count(&self) -> usize {
        self.users.len()
    }

    pub fn set_superuser(&mut self, username: &str, is_super: bool) -> bool {
        if let Some(user) = self.users.get_mut(username) {
            user.is_superuser = is_super;
            true
        } else {
            false
        }
    }

    // ── Role assignment ───────────────────

    pub fn grant_role(&mut self, username: &str, role_name: &str) -> bool {
        if !self.roles.contains_key(role_name) {
            return false;
        }
        if let Some(user) = self.users.get_mut(username) {
            user.roles.insert(role_name.to_string());
            true
        } else {
            false
        }
    }

    pub fn revoke_role(&mut self, username: &str, role_name: &str) -> bool {
        if let Some(user) = self.users.get_mut(username) {
            user.roles.remove(role_name)
        } else {
            false
        }
    }

    // ── Authorization check ───────────────

    /// Check if a user has a specific privilege on a table.
    pub fn check_privilege(&self, username: &str, table: &str, priv_: Privilege) -> bool {
        let user = match self.users.get(username) {
            Some(u) => u,
            None => return false,
        };

        // Superuser bypass
        if user.is_superuser {
            return true;
        }

        // Direct global privileges
        if user.direct_privileges.contains(&priv_)
            || user.direct_privileges.contains(&Privilege::All)
        {
            return true;
        }

        // Direct table privileges
        if let Some(tbl_privs) = user.direct_table_privileges.get(table) {
            if tbl_privs.contains(&priv_) || tbl_privs.contains(&Privilege::All) {
                return true;
            }
        }

        // Role-based privileges
        for role_name in &user.roles {
            if let Some(role) = self.roles.get(role_name) {
                if role.has_privilege(table, priv_) {
                    return true;
                }
            }
        }

        false
    }

    /// Grant a global privilege directly to a user.
    pub fn grant_direct(&mut self, username: &str, priv_: Privilege) -> bool {
        if let Some(user) = self.users.get_mut(username) {
            if priv_ == Privilege::All {
                for p in Privilege::all_concrete() {
                    user.direct_privileges.insert(*p);
                }
            } else {
                user.direct_privileges.insert(priv_);
            }
            true
        } else {
            false
        }
    }

    /// Grant a table-specific privilege directly to a user.
    pub fn grant_direct_table(&mut self, username: &str, table: &str, priv_: Privilege) -> bool {
        if let Some(user) = self.users.get_mut(username) {
            let entry = user
                .direct_table_privileges
                .entry(table.to_string())
                .or_default();
            if priv_ == Privilege::All {
                for p in Privilege::all_concrete() {
                    entry.insert(*p);
                }
            } else {
                entry.insert(priv_);
            }
            true
        } else {
            false
        }
    }

    /// List all roles.
    pub fn roles(&self) -> Vec<&str> {
        self.roles.keys().map(|s| s.as_str()).collect()
    }

    /// List all users.
    pub fn users(&self) -> Vec<&str> {
        self.users.keys().map(|s| s.as_str()).collect()
    }
}

// ── Audit Persistence ─────────────────────────────────────────────────

/// In-memory buffer for audit log entries that can be flushed to disk.
///
/// This provides a lightweight write-ahead buffer that collects audit
/// entries in memory and can serialize them to a file.
pub struct AuditPersistence {
    buffer: Vec<String>,
    max_buffer_size: usize,
    total_flushed: usize,
}

impl AuditPersistence {
    pub fn new(max_buffer_size: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer_size,
            total_flushed: 0,
        }
    }

    /// Append a formatted audit entry to the buffer.
    pub fn append(&mut self, entry: String) -> bool {
        self.buffer.push(entry);
        self.buffer.len() >= self.max_buffer_size
    }

    /// Get current buffer contents.
    pub fn buffer(&self) -> &[String] {
        &self.buffer
    }

    /// Flush the buffer — returns the entries and marks them as flushed.
    ///
    /// In production this would write to a file; here we just drain the buffer.
    pub fn flush(&mut self) -> Vec<String> {
        self.total_flushed += self.buffer.len();
        std::mem::take(&mut self.buffer)
    }

    /// Number of entries buffered.
    pub fn buffered_count(&self) -> usize {
        self.buffer.len()
    }

    /// Total entries flushed since creation.
    pub fn total_flushed(&self) -> usize {
        self.total_flushed
    }

    /// Whether the buffer has reached its capacity.
    pub fn is_full(&self) -> bool {
        self.buffer.len() >= self.max_buffer_size
    }

    /// Clear the buffer without counting as flushed.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_parse() {
        assert_eq!(Privilege::from_str_name("select"), Some(Privilege::Select));
        assert_eq!(Privilege::from_str_name("INSERT"), Some(Privilege::Insert));
        assert_eq!(Privilege::from_str_name("ALL"), Some(Privilege::All));
        assert_eq!(Privilege::from_str_name("bogus"), None);
    }

    #[test]
    fn privilege_display() {
        assert_eq!(format!("{}", Privilege::Select), "SELECT");
        assert_eq!(format!("{}", Privilege::All), "ALL");
    }

    #[test]
    fn role_grant_global() {
        let mut role = Role::new("admin");
        role.grant_global(Privilege::Select);
        assert!(role.has_privilege("any_table", Privilege::Select));
        assert!(!role.has_privilege("any_table", Privilege::Delete));
    }

    #[test]
    fn role_grant_all() {
        let mut role = Role::new("admin");
        role.grant_global(Privilege::All);
        assert!(role.has_privilege("t", Privilege::Select));
        assert!(role.has_privilege("t", Privilege::Insert));
        assert!(role.has_privilege("t", Privilege::Delete));
    }

    #[test]
    fn role_grant_table() {
        let mut role = Role::new("analyst");
        role.grant_table("sales", Privilege::Select);
        assert!(role.has_privilege("sales", Privilege::Select));
        assert!(!role.has_privilege("users", Privilege::Select));
    }

    #[test]
    fn role_revoke() {
        let mut role = Role::new("editor");
        role.grant_global(Privilege::Insert);
        role.grant_global(Privilege::Update);
        role.revoke_global(Privilege::Insert);
        assert!(!role.has_privilege("t", Privilege::Insert));
        assert!(role.has_privilege("t", Privilege::Update));
    }

    #[test]
    fn role_effective_privileges() {
        let mut role = Role::new("mixed");
        role.grant_global(Privilege::Select);
        role.grant_table("orders", Privilege::Insert);
        let eff = role.effective_privileges("orders");
        assert!(eff.contains(&Privilege::Select));
        assert!(eff.contains(&Privilege::Insert));
    }

    #[test]
    fn rbac_user_role_workflow() {
        let mut mgr = RbacManager::new();
        mgr.create_user("alice");
        mgr.create_role("reader");
        mgr.get_role_mut("reader")
            .unwrap()
            .grant_global(Privilege::Select);
        mgr.grant_role("alice", "reader");

        assert!(mgr.check_privilege("alice", "t1", Privilege::Select));
        assert!(!mgr.check_privilege("alice", "t1", Privilege::Insert));
    }

    #[test]
    fn rbac_superuser_bypass() {
        let mut mgr = RbacManager::new();
        mgr.create_user("root");
        mgr.set_superuser("root", true);
        assert!(mgr.check_privilege("root", "anything", Privilege::Delete));
        assert!(mgr.check_privilege("root", "anything", Privilege::Drop));
    }

    #[test]
    fn rbac_direct_table_privilege() {
        let mut mgr = RbacManager::new();
        mgr.create_user("bob");
        mgr.grant_direct_table("bob", "orders", Privilege::Insert);
        assert!(mgr.check_privilege("bob", "orders", Privilege::Insert));
        assert!(!mgr.check_privilege("bob", "users", Privilege::Insert));
    }

    #[test]
    fn rbac_unknown_user() {
        let mgr = RbacManager::new();
        assert!(!mgr.check_privilege("nonexistent", "t", Privilege::Select));
    }

    #[test]
    fn rbac_drop_role_removes_from_users() {
        let mut mgr = RbacManager::new();
        mgr.create_user("alice");
        mgr.create_role("admin");
        mgr.grant_role("alice", "admin");
        mgr.drop_role("admin");
        assert!(!mgr.get_user("alice").unwrap().roles.contains("admin"));
    }

    #[test]
    fn audit_persistence_buffer_and_flush() {
        let mut ap = AuditPersistence::new(3);
        assert!(!ap.append("entry1".to_string()));
        assert!(!ap.append("entry2".to_string()));
        assert!(ap.append("entry3".to_string())); // full
        assert!(ap.is_full());
        let flushed = ap.flush();
        assert_eq!(flushed.len(), 3);
        assert_eq!(ap.buffered_count(), 0);
        assert_eq!(ap.total_flushed(), 3);
    }

    #[test]
    fn audit_persistence_clear() {
        let mut ap = AuditPersistence::new(10);
        ap.append("x".to_string());
        ap.clear();
        assert_eq!(ap.buffered_count(), 0);
        assert_eq!(ap.total_flushed(), 0);
    }
}
