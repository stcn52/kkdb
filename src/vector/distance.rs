/// Distance metrics for vector similarity computation.
///
/// Mirrors the role of `fulltext/tokenizer.rs` in the FTS subsystem.
///
/// Distance metric selector stored on each vector index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DistanceMetric {
    /// Cosine similarity — recommended for embedding vectors (normalised or not).
    /// Returns a value in [‑1, 1]; higher = more similar.
    Cosine,
    /// Euclidean (L2) distance.
    /// Returns a non-negative value; lower = more similar.
    L2,
}

impl DistanceMetric {
    /// Parse from a string produced by `CREATE VECTOR INDEX … DISTANCE <metric>`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "COSINE" | "COS" => Some(Self::Cosine),
            "L2" | "EUCLIDEAN" => Some(Self::L2),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        match self {
            Self::Cosine => 0x01,
            Self::L2 => 0x02,
        }
    }

    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Self::Cosine),
            0x02 => Some(Self::L2),
            _ => None,
        }
    }

    /// Compute a *similarity* score (higher = more similar) between two vectors.
    ///
    /// For `Cosine`, returns cosine similarity in [‑1, 1].
    /// For `L2`, returns `1 / (1 + distance)` so that closer = higher score.
    pub fn similarity(&self, a: &[f32], b: &[f32]) -> f32 {
        match self {
            Self::Cosine => cosine_similarity(a, b),
            Self::L2 => {
                let d = l2_distance(a, b);
                1.0 / (1.0 + d)
            }
        }
    }

    /// Raw distance (lower = more similar, used for HNSW internal comparisons).
    pub fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self {
            Self::Cosine => 1.0 - cosine_similarity(a, b),
            Self::L2 => l2_distance(a, b),
        }
    }
}

// ─── Core math ───────────────────────────────────────────────────────────────

/// Dot product of two equal-length slices.
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// L2 (Euclidean) norm of a vector.
#[inline]
pub fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Cosine similarity between two vectors.
///
/// Returns 0.0 if either vector is a zero vector (degenerate case).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let a = &a[..len];
    let b = &b[..len];
    let dot = dot_product(a, b);
    let norm_a = l2_norm(a);
    let norm_b = l2_norm(b);
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
    }
}

/// Euclidean (L2) distance between two vectors.
pub fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    a[..len]
        .iter()
        .zip(b[..len].iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

/// L2-normalise a vector in-place.
pub fn normalize_l2(v: &mut [f32]) {
    let n = l2_norm(v);
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical() {
        let a = vec![1.0f32, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn test_l2_zero() {
        let a = vec![1.0f32, 2.0, 3.0];
        assert!((l2_distance(&a, &a)).abs() < 1e-6);
    }

    #[test]
    fn test_normalize() {
        let mut v = vec![3.0f32, 4.0];
        normalize_l2(&mut v);
        assert!((l2_norm(&v) - 1.0).abs() < 1e-6);
    }

    // ── New coverage tests ──────────────────────────────────────────────

    #[test]
    fn test_cosine_zero_vector() {
        let zero = vec![0.0f32, 0.0, 0.0];
        let nonzero = vec![1.0f32, 0.0, 0.0];
        assert_eq!(cosine_similarity(&zero, &nonzero), 0.0);
        assert_eq!(cosine_similarity(&zero, &zero), 0.0);
    }

    #[test]
    fn test_cosine_nan_vector() {
        let nan_vec = vec![f32::NAN, 1.0];
        let normal = vec![1.0f32, 0.0];
        let sim = cosine_similarity(&nan_vec, &normal);
        // NaN propagation: result should be NaN or clamped; either way no panic
        assert!(!sim.is_infinite());
    }

    #[test]
    fn test_normalize_zero_vector() {
        let mut v = vec![0.0f32, 0.0, 0.0];
        normalize_l2(&mut v); // should not panic (divide by zero)
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn test_l2_distance_different_lengths() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![1.0f32, 2.0];
        // Should use min(len) = 2 and compute distance on first 2 elements
        let d = l2_distance(&a, &b);
        assert!(d.abs() < 1e-6, "first 2 elements are identical; got {}", d);
    }

    #[test]
    fn test_dot_product_basic() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![4.0f32, 5.0, 6.0];
        let d = dot_product(&a, &b);
        assert!((d - 32.0).abs() < 1e-6);
    }

    #[test]
    fn test_l2_norm_basic() {
        let v = vec![3.0f32, 4.0];
        assert!((l2_norm(&v) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_from_str() {
        assert_eq!(DistanceMetric::from_str("COSINE"), Some(DistanceMetric::Cosine));
        assert_eq!(DistanceMetric::from_str("cos"), Some(DistanceMetric::Cosine));
        assert_eq!(DistanceMetric::from_str("L2"), Some(DistanceMetric::L2));
        assert_eq!(DistanceMetric::from_str("euclidean"), Some(DistanceMetric::L2));
        assert_eq!(DistanceMetric::from_str("unknown"), None);
    }

    #[test]
    fn test_distance_metric_u8_round_trip() {
        for m in &[DistanceMetric::Cosine, DistanceMetric::L2] {
            assert_eq!(DistanceMetric::from_u8(m.as_u8()), Some(*m));
        }
        assert_eq!(DistanceMetric::from_u8(0x00), None);
        assert_eq!(DistanceMetric::from_u8(0xFF), None);
    }

    #[test]
    fn test_distance_metric_similarity_vs_distance() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        // For orthogonal vectors:cosine similarity ≈ 0, distance ≈ 1
        let sim = DistanceMetric::Cosine.similarity(&a, &b);
        let dist = DistanceMetric::Cosine.distance(&a, &b);
        assert!(sim.abs() < 1e-6);
        assert!((dist - 1.0).abs() < 1e-6);
    }
}
