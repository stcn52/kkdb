// ── src/fulltext/fts_advanced.rs ──
// R21: 全文检索增强 — 模糊搜索 / 同义词扩展 / 分面搜索 / 实时索引更新

use std::collections::{HashMap, HashSet, VecDeque};

// ═══════════════════════════════════════════════════════════════════════
// 1. FuzzySearcher — 模糊搜索（编辑距离）
// ═══════════════════════════════════════════════════════════════════════

/// 模糊匹配结果
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    pub term: String,
    pub distance: usize,
    pub doc_ids: Vec<u64>,
}

/// 模糊搜索器 — 基于编辑距离的近似匹配
pub struct FuzzySearcher {
    max_distance: usize,
    dictionary: Vec<String>,
    searches: u64,
}

impl FuzzySearcher {
    pub fn new(max_distance: usize) -> Self {
        Self {
            max_distance,
            dictionary: Vec::new(),
            searches: 0,
        }
    }

    pub fn add_term(&mut self, term: &str) {
        if !self.dictionary.contains(&term.to_string()) {
            self.dictionary.push(term.to_string());
        }
    }

    pub fn add_terms(&mut self, terms: &[&str]) {
        for t in terms {
            self.add_term(t);
        }
    }

    /// 计算编辑距离（Levenshtein）
    pub fn edit_distance(a: &str, b: &str) -> usize {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let m = a_chars.len();
        let n = b_chars.len();

        let mut dp = vec![vec![0usize; n + 1]; m + 1];
        for i in 0..=m {
            dp[i][0] = i;
        }
        for j in 0..=n {
            dp[0][j] = j;
        }
        for i in 1..=m {
            for j in 1..=n {
                let cost = if a_chars[i - 1] == b_chars[j - 1] {
                    0
                } else {
                    1
                };
                dp[i][j] = (dp[i - 1][j] + 1)
                    .min(dp[i][j - 1] + 1)
                    .min(dp[i - 1][j - 1] + cost);
            }
        }
        dp[m][n]
    }

    /// 查找模糊匹配
    pub fn search(&mut self, query: &str) -> Vec<FuzzyMatch> {
        self.searches += 1;
        let mut matches: Vec<FuzzyMatch> = Vec::new();
        for term in &self.dictionary {
            let dist = Self::edit_distance(query, term);
            if dist <= self.max_distance {
                matches.push(FuzzyMatch {
                    term: term.clone(),
                    distance: dist,
                    doc_ids: Vec::new(),
                });
            }
        }
        matches.sort_by_key(|m| m.distance);
        matches
    }

    pub fn dictionary_size(&self) -> usize {
        self.dictionary.len()
    }

    pub fn search_count(&self) -> u64 {
        self.searches
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. SynonymExpander — 同义词扩展引擎
// ═══════════════════════════════════════════════════════════════════════

/// 同义词组
#[derive(Debug, Clone)]
pub struct SynonymGroup {
    pub canonical: String,
    pub synonyms: HashSet<String>,
}

/// 同义词扩展器
pub struct SynonymExpander {
    groups: Vec<SynonymGroup>,
    lookup: HashMap<String, usize>, // term -> group index
}

impl SynonymExpander {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    pub fn add_group(&mut self, canonical: &str, synonyms: Vec<&str>) {
        let idx = self.groups.len();
        let mut syn_set: HashSet<String> = synonyms.iter().map(|s| s.to_string()).collect();
        syn_set.insert(canonical.to_string());

        for s in &syn_set {
            self.lookup.insert(s.to_lowercase(), idx);
        }

        self.groups.push(SynonymGroup {
            canonical: canonical.to_string(),
            synonyms: syn_set,
        });
    }

    /// 扩展查询词为所有同义词
    pub fn expand(&self, term: &str) -> Vec<String> {
        let lower = term.to_lowercase();
        match self.lookup.get(&lower) {
            Some(&idx) => self.groups[idx].synonyms.iter().cloned().collect(),
            None => vec![term.to_string()],
        }
    }

    /// 扩展多个查询词
    pub fn expand_query(&self, terms: &[&str]) -> Vec<String> {
        let mut expanded: Vec<String> = Vec::new();
        for term in terms {
            for syn in self.expand(term) {
                if !expanded.contains(&syn) {
                    expanded.push(syn);
                }
            }
        }
        expanded
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn total_terms(&self) -> usize {
        self.lookup.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. FacetedSearch — 分面搜索
// ═══════════════════════════════════════════════════════════════════════

/// 分面值计数
#[derive(Debug, Clone)]
pub struct FacetCount {
    pub value: String,
    pub count: usize,
}

/// 分面定义
#[derive(Debug, Clone)]
pub struct FacetField {
    pub field_name: String,
    pub values: HashMap<String, usize>,
}

impl FacetField {
    pub fn new(field_name: &str) -> Self {
        Self {
            field_name: field_name.to_string(),
            values: HashMap::new(),
        }
    }

    pub fn add_value(&mut self, value: &str) {
        *self.values.entry(value.to_string()).or_insert(0) += 1;
    }

    pub fn top_values(&self, n: usize) -> Vec<FacetCount> {
        let mut counts: Vec<FacetCount> = self
            .values
            .iter()
            .map(|(v, &c)| FacetCount {
                value: v.clone(),
                count: c,
            })
            .collect();
        counts.sort_by(|a, b| b.count.cmp(&a.count));
        counts.truncate(n);
        counts
    }

    pub fn unique_values(&self) -> usize {
        self.values.len()
    }
}

/// 分面搜索管理器
pub struct FacetedSearchManager {
    facets: HashMap<String, FacetField>,
    result_count: usize,
}

impl FacetedSearchManager {
    pub fn new() -> Self {
        Self {
            facets: HashMap::new(),
            result_count: 0,
        }
    }

    pub fn define_facet(&mut self, field: &str) {
        self.facets
            .insert(field.to_string(), FacetField::new(field));
    }

    pub fn index_document(&mut self, facet_values: &[(&str, &str)]) {
        self.result_count += 1;
        for (field, value) in facet_values {
            if let Some(facet) = self.facets.get_mut(*field) {
                facet.add_value(value);
            }
        }
    }

    pub fn get_facet(&self, field: &str) -> Option<&FacetField> {
        self.facets.get(field)
    }

    pub fn facet_count(&self) -> usize {
        self.facets.len()
    }

    pub fn document_count(&self) -> usize {
        self.result_count
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. RealTimeIndexer — 实时索引更新
// ═══════════════════════════════════════════════════════════════════════

/// 索引操作
#[derive(Debug, Clone)]
pub enum IndexOp {
    Insert { doc_id: u64, terms: Vec<String> },
    Delete { doc_id: u64 },
    Update { doc_id: u64, terms: Vec<String> },
}

/// 实时索引更新器
pub struct RealTimeIndexer {
    pending_ops: VecDeque<IndexOp>,
    batch_threshold: usize,
    indexed_docs: HashSet<u64>,
    ops_applied: u64,
    batches_flushed: u64,
}

impl RealTimeIndexer {
    pub fn new(batch_threshold: usize) -> Self {
        Self {
            pending_ops: VecDeque::new(),
            batch_threshold,
            indexed_docs: HashSet::new(),
            ops_applied: 0,
            batches_flushed: 0,
        }
    }

    pub fn enqueue(&mut self, op: IndexOp) {
        self.pending_ops.push_back(op);
    }

    /// 是否应当刷新
    pub fn should_flush(&self) -> bool {
        self.pending_ops.len() >= self.batch_threshold
    }

    /// 刷新待处理操作
    pub fn flush(&mut self) -> Vec<IndexOp> {
        let ops: Vec<IndexOp> = self.pending_ops.drain(..).collect();
        for op in &ops {
            match op {
                IndexOp::Insert { doc_id, .. } => {
                    self.indexed_docs.insert(*doc_id);
                }
                IndexOp::Delete { doc_id } => {
                    self.indexed_docs.remove(doc_id);
                }
                IndexOp::Update { doc_id, .. } => {
                    self.indexed_docs.insert(*doc_id);
                }
            }
            self.ops_applied += 1;
        }
        self.batches_flushed += 1;
        ops
    }

    pub fn pending_count(&self) -> usize {
        self.pending_ops.len()
    }

    pub fn indexed_doc_count(&self) -> usize {
        self.indexed_docs.len()
    }

    pub fn ops_applied(&self) -> u64 {
        self.ops_applied
    }

    pub fn batches_flushed(&self) -> u64 {
        self.batches_flushed
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_edit_distance() {
        assert_eq!(FuzzySearcher::edit_distance("kitten", "sitting"), 3);
        assert_eq!(FuzzySearcher::edit_distance("", "abc"), 3);
        assert_eq!(FuzzySearcher::edit_distance("same", "same"), 0);
    }

    #[test]
    fn test_fuzzy_search_matches() {
        let mut fs = FuzzySearcher::new(2);
        fs.add_terms(&["apple", "application", "apply", "banana", "appeal"]);
        let results = fs.search("aple");
        assert!(results.iter().any(|m| m.term == "apple" && m.distance == 1));
        assert_eq!(fs.dictionary_size(), 5);
    }

    #[test]
    fn test_fuzzy_no_match() {
        let mut fs = FuzzySearcher::new(1);
        fs.add_term("hello");
        let results = fs.search("world");
        assert!(results.is_empty());
    }

    #[test]
    fn test_synonym_expand() {
        let mut se = SynonymExpander::new();
        se.add_group("car", vec!["automobile", "vehicle", "auto"]);
        let expanded = se.expand("automobile");
        assert!(expanded.contains(&"car".to_string()));
        assert!(expanded.contains(&"vehicle".to_string()));
        assert!(expanded.len() >= 4);
    }

    #[test]
    fn test_synonym_expand_unknown() {
        let se = SynonymExpander::new();
        let expanded = se.expand("unknown");
        assert_eq!(expanded, vec!["unknown".to_string()]);
    }

    #[test]
    fn test_synonym_expand_query() {
        let mut se = SynonymExpander::new();
        se.add_group("fast", vec!["quick", "rapid"]);
        se.add_group("big", vec!["large", "huge"]);
        let expanded = se.expand_query(&["fast", "house"]);
        assert!(expanded.contains(&"quick".to_string()));
        assert!(expanded.contains(&"house".to_string()));
    }

    #[test]
    fn test_faceted_search() {
        let mut mgr = FacetedSearchManager::new();
        mgr.define_facet("category");
        mgr.define_facet("brand");

        mgr.index_document(&[("category", "electronics"), ("brand", "Apple")]);
        mgr.index_document(&[("category", "electronics"), ("brand", "Samsung")]);
        mgr.index_document(&[("category", "clothing"), ("brand", "Nike")]);

        let cat = mgr.get_facet("category").unwrap();
        assert_eq!(cat.unique_values(), 2);
        let top = cat.top_values(1);
        assert_eq!(top[0].value, "electronics");
        assert_eq!(top[0].count, 2);
        assert_eq!(mgr.document_count(), 3);
    }

    #[test]
    fn test_faceted_search_top_n() {
        let mut field = FacetField::new("color");
        for _ in 0..5 {
            field.add_value("red");
        }
        for _ in 0..3 {
            field.add_value("blue");
        }
        for _ in 0..1 {
            field.add_value("green");
        }
        let top = field.top_values(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].value, "red");
        assert_eq!(top[1].value, "blue");
    }

    #[test]
    fn test_realtime_indexer_lifecycle() {
        let mut idx = RealTimeIndexer::new(3);
        idx.enqueue(IndexOp::Insert {
            doc_id: 1,
            terms: vec!["hello".into()],
        });
        idx.enqueue(IndexOp::Insert {
            doc_id: 2,
            terms: vec!["world".into()],
        });
        assert!(!idx.should_flush());
        idx.enqueue(IndexOp::Insert {
            doc_id: 3,
            terms: vec!["foo".into()],
        });
        assert!(idx.should_flush());

        let ops = idx.flush();
        assert_eq!(ops.len(), 3);
        assert_eq!(idx.indexed_doc_count(), 3);
        assert_eq!(idx.batches_flushed(), 1);
    }

    #[test]
    fn test_realtime_indexer_delete() {
        let mut idx = RealTimeIndexer::new(10);
        idx.enqueue(IndexOp::Insert {
            doc_id: 1,
            terms: vec!["a".into()],
        });
        idx.enqueue(IndexOp::Insert {
            doc_id: 2,
            terms: vec!["b".into()],
        });
        idx.flush();
        assert_eq!(idx.indexed_doc_count(), 2);

        idx.enqueue(IndexOp::Delete { doc_id: 1 });
        idx.flush();
        assert_eq!(idx.indexed_doc_count(), 1);
        assert_eq!(idx.ops_applied(), 3);
    }
}
