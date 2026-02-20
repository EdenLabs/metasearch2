use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::LazyLock;

use eyre::Context;
use regex::Regex;
use tracing::{info, warn};
use unicode_normalization::UnicodeNormalization;
use url::Url;

use crate::config::{L2FeatureWeights, RerankConfig, UrlSignalWeights};
use super::{EngineSearchResult, SearchResult};

// --- RerankData ---

/// Immutable reranking data loaded at startup.
/// Holds the domain blocklist and reputation weights used by
/// the two-layer scoring system to re-order search results.
pub struct RerankData {
    /// Domains that should be excluded from results entirely.
    blocklist  : HashSet<String>,
    /// Per-domain reputation weights parsed from the reputation TOML.
    /// Higher values boost the domain, 0.0 blocks it.
    reputation : HashMap<String, f64>,
}

impl RerankData {
    /// Loads blocklist and reputation data from the paths specified
    /// in the config. Falls back to empty data if files are missing
    /// so the server can still start without them.
    pub fn load(config: &RerankConfig) -> eyre::Result<Self> {
        // Load the blocklist file.
        let blocklist = Self::load_blocklist(&config.blocklist_path)?;
        info!(
            "loaded {} blocked domains from '{}'",
            blocklist.len(),
            config.blocklist_path,
        );

        // Load the reputation TOML.
        let reputation = Self::load_reputation(&config.reputation_path)?;
        info!(
            "loaded {} reputation entries from '{}'",
            reputation.len(),
            config.reputation_path,
        );

        Ok(Self {
            blocklist:  blocklist,
            reputation: reputation,
        })
    }

    /// Looks up the reputation weight for a domain.
    /// Tries exact match first, then walks up subdomains
    /// (e.g. docs.rs -> rs). Returns 1.0 for unknown domains.
    /// Reputation entries take priority over the blocklist, so a
    /// domain with a non-block TOML entry is never treated as blocked.
    pub fn get_domain_weight(&self, domain: &str) -> f64 {
        let mut current = domain;

        loop {
            // Reputation map takes priority over blocklist.
            if let Some(&weight) = self.reputation.get(current) {
                return weight;
            }

            if self.blocklist.contains(current) {
                return 0.0;
            }

            // Strip the leftmost label and try the parent domain.
            match current.find('.') {
                Some(pos) => current = &current[pos + 1..],
                None      => break,
            }
        }

        // Not found anywhere, treat as neutral.
        1.0
    }

    /// Reads the blocklist file line by line. Skips comments (lines
    /// starting with '#') and blank lines.
    fn load_blocklist(path: &str) -> eyre::Result<HashSet<String>> {
        let contents = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warn!("blocklist file not found at '{}', using empty blocklist", path);
                return Ok(HashSet::new());
            }
            Err(e) => {
                return Err(e).wrap_err_with(
                    || format!("failed to read blocklist from '{}'", path),
                );
            }
        };

        let set = contents
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.to_string())
            .collect();

        Ok(set)
    }

    /// Parses the reputation TOML file.
    ///
    /// Expected format:
    /// ```toml
    /// [action.category]
    /// domains = ["domain1.com", "domain2.com"]
    /// ```
    ///
    /// Action names map to weights: pin=2.0, raise=1.5,
    /// neutral=1.0, lower=0.3, block=0.0.
    fn load_reputation(path: &str) -> eyre::Result<HashMap<String, f64>> {
        let contents = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warn!(
                    "reputation file not found at '{}', using empty reputation",
                    path,
                );
                return Ok(HashMap::new());
            }
            Err(e) => {
                return Err(e).wrap_err_with(
                    || format!("failed to read reputation from '{}'", path),
                );
            }
        };

        let table: toml::Table = contents.parse::<toml::Table>()
            .wrap_err("failed to parse reputation TOML")?;

        let mut map = HashMap::new();

        for (action_name, action_value) in &table {
            let weight = match action_name.as_str() {
                "pin"     => 2.0,
                "raise"   => 1.5,
                "neutral" => 1.0,
                "lower"   => 0.3,
                "block"   => 0.0,
                other => {
                    warn!("unknown reputation action '{}', skipping", other);
                    continue;
                }
            };

            // Each action contains category sub-tables.
            let Some(categories) = action_value.as_table()
            else {
                warn!("expected table for action '{}', skipping", action_name);
                continue;
            };

            for (category_name, category_value) in categories {
                let Some(cat_table) = category_value.as_table()
                else {
                    warn!(
                        "expected table for category '{}.{}', skipping",
                        action_name, category_name,
                    );
                    continue;
                };

                let Some(domains_val) = cat_table.get("domains")
                else {
                    warn!(
                        "no 'domains' key in '{}.{}', skipping",
                        action_name, category_name,
                    );
                    continue;
                };

                let Some(domains_arr) = domains_val.as_array()
                else {
                    warn!(
                        "'domains' is not an array in '{}.{}', skipping",
                        action_name, category_name,
                    );
                    continue;
                };

                for domain_val in domains_arr {
                    if let Some(domain) = domain_val.as_str() {
                        map.insert(domain.to_string(), weight);
                    }
                }
            }
        }

        Ok(map)
    }
}

// --- Layer 1 ---

/// Regex matching commercial/marketing domain patterns. Used as a
/// negative signal in URL structure scoring.
static COMMERCIAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(store|shop|buy|order|supply|supplies|mart|outlet|deals|pricing",
        r"|law|legal|attorney|lawyer|consult|clinic|therapy|advisor",
        r"|seo|marketing|agency|growth|leadgen|funnel)",
    ))
    .unwrap()
});

impl RerankData {
    /// Computes the Layer 1 source identity score for a URL.
    /// Returns None if the domain is blocked (weight 0.0).
    /// Returns Some(score) combining reputation modifier and URL
    /// structure signals otherwise.
    pub fn score_l1(
        &self,
        url     : &str,
        weights : &UrlSignalWeights,
    )
        -> Option<f64>
    {
        // Parse the URL. If it fails, return a neutral score.
        let parsed = match Url::parse(url) {
            Ok(u)  => u,
            Err(_) => return Some(0.0),
        };

        let domain = match extract_domain_from_parsed(&parsed) {
            Some(d) => d,
            None    => return Some(0.0),
        };

        // Look up reputation weight. Block if zero.
        let rep_weight = self.get_domain_weight(&domain);
        if rep_weight == 0.0 {
            return None;
        }

        // Convert reputation weight to a modifier centered on zero.
        let reputation_modifier = (rep_weight - 1.0).clamp(-0.5, 1.0);

        // Compute URL structure signals and combine with weights.
        let url_signals = compute_url_signals(&parsed, &domain, weights);

        Some(reputation_modifier + url_signals)
    }
}

/// Extracts the domain from a URL string, stripping the `www.` prefix.
/// Returns None if the URL cannot be parsed or has no host.
#[allow(dead_code)]
fn extract_domain(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    extract_domain_from_parsed(&parsed)
}

/// Extracts the domain from an already-parsed URL, stripping `www.`.
fn extract_domain_from_parsed(parsed: &Url) -> Option<String> {
    let host = parsed.host_str()?;
    let domain = host.strip_prefix("www.").unwrap_or(host);
    Some(domain.to_string())
}

/// Computes URL structure signals and returns a weighted sum
/// normalized to approximately [-0.5, 0.5].
fn compute_url_signals(
    parsed  : &Url,
    domain  : &str,
    weights : &UrlSignalWeights,
)
    -> f64
{
    let path = parsed.path();

    // Signal: URL depth. Count path segments (slashes), normalize.
    let depth = path.matches('/').count().saturating_sub(1);
    let sig_depth = (depth as f64 / 5.0).clamp(0.0, 1.0);

    // Signal: word count in the last path segment (slug).
    let sig_slug_word_count = {
        let last_segment = path.rsplit('/').next().unwrap_or("");
        let count = last_segment
            .split(|c: char| c == '-' || c == '_')
            .filter(|s| !s.is_empty())
            .count();
        (count as f64 / 8.0).clamp(0.0, 1.0)
    };

    // Signal: documentation path patterns.
    let sig_docs_path = {
        let lower = path.to_lowercase();
        let is_docs = lower.contains("/docs/")
            || lower.contains("/api/")
            || lower.contains("/reference/")
            || lower.contains("/manual/")
            || lower.contains("/wiki/")
            || lower.contains("/spec/");
        if is_docs { 1.0 } else { 0.0 }
    };

    // Signal: forum/community path patterns.
    let sig_forum_path = {
        let lower = path.to_lowercase();
        let is_forum = lower.contains("/forum/")
            || lower.contains("/discuss/")
            || lower.contains("/community/")
            || lower.contains("/questions/")
            || lower.contains("/answers/");
        if is_forum { 1.0 } else { 0.0 }
    };

    // Signal: institutional TLD.
    let sig_institutional_tld = {
        let is_inst = domain.ends_with(".edu")
            || domain.ends_with(".gov")
            || domain.ends_with(".org")
            || domain.ends_with(".mil");
        if is_inst { 1.0 } else { 0.0 }
    };

    // Signal: domain token count (dots and hyphens as separators).
    let sig_domain_token_count = {
        let count = domain
            .chars()
            .filter(|&c| c == '.' || c == '-')
            .count();
        (count as f64 / 5.0).clamp(0.0, 1.0)
    };

    // Signal: has a non-www subdomain (more than 2 labels).
    let sig_has_subdomain = {
        let label_count = domain.split('.').count();
        if label_count > 2 { 1.0 } else { 0.0 }
    };

    // Signal: commercial domain pattern.
    let sig_commercial = {
        if COMMERCIAL_RE.is_match(domain) { 1.0 } else { 0.0 }
    };

    // Weighted sum of all signals.
    let raw = sig_depth            * weights.url_depth
        + sig_slug_word_count      * weights.url_slug_word_count
        + sig_docs_path            * weights.is_docs_path
        + sig_forum_path           * weights.is_forum_path
        + sig_institutional_tld    * weights.is_institutional_tld
        + sig_domain_token_count   * weights.domain_token_count
        + sig_has_subdomain        * weights.has_subdomain
        + sig_commercial           * weights.is_commercial_pattern;

    // Normalize by the theoretical maximum magnitude so the result
    // stays roughly within [-0.5, 0.5].
    let max_possible = weights.url_depth.abs()
        + weights.url_slug_word_count.abs()
        + weights.is_docs_path.abs()
        + weights.is_forum_path.abs()
        + weights.is_institutional_tld.abs()
        + weights.domain_token_count.abs()
        + weights.has_subdomain.abs()
        + weights.is_commercial_pattern.abs();

    if max_possible > 0.0 {
        (raw / max_possible).clamp(-0.5, 0.5)
    }
    else {
        0.0
    }
}

// --- Layer 2 ---

/// Computes the Layer 2 audience sophistication score from snippet text.
/// Returns a value in [0.0, 1.0]. Short snippets (fewer than 3 words)
/// return 0.5 as a neutral default. This is a free function because it
/// does not depend on blocklist or reputation data.
pub fn score_l2(text: &str, weights: &L2FeatureWeights) -> f64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    let total = words.len();

    if total < 3 {
        return 0.5;
    }

    // Feature: capitalized word ratio.
    // Skip the first word since it starts a sentence.
    let capitalized_count = words[1..]
        .iter()
        .filter(|w| {
            w.chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
        })
        .count();
    let cap_ratio = capitalized_count as f64 / (total - 1).max(1) as f64;
    let feat_cap = normalize(cap_ratio, 0.0, 0.45);

    // Feature: unique word ratio.
    let unique: HashSet<String> = words
        .iter()
        .map(|w| w.to_lowercase())
        .collect();
    let unique_ratio = unique.len() as f64 / total as f64;
    let feat_unique = normalize(unique_ratio, 0.40, 1.0);

    // Feature: technical term density (words with >= 8 chars).
    let long_word_count = words
        .iter()
        .filter(|w| w.len() >= 8)
        .count();
    let tech_density = long_word_count as f64 / total as f64;
    let feat_tech = normalize(tech_density, 0.0, 0.50);

    // Feature: average word length.
    let total_chars: usize = words.iter().map(|w| w.len()).sum();
    let avg_len = total_chars as f64 / total as f64;
    let feat_avg_len = normalize(avg_len, 3.5, 8.0);

    // Feature: characters per word (same computation, different
    // normalization range).
    let chars_per_word = total_chars as f64 / total as f64;
    let feat_cpw = normalize(chars_per_word, 3.5, 9.0);

    // Weighted sum, clamped to [0, 1].
    let score = feat_cap  * weights.capitalized_word_ratio
        + feat_unique     * weights.unique_word_ratio
        + feat_tech       * weights.technical_term_density
        + feat_avg_len    * weights.avg_word_length
        + feat_cpw        * weights.char_per_word;

    score.clamp(0.0, 1.0)
}

/// Linear normalization into [0, 1] given a value and min/max range.
fn normalize(value: f64, min: f64, max: f64) -> f64 {
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

// --- Query Relevance ---

/// Extracts byte trigrams from a normalized, lowercased string.
/// NFD-decomposes then strips combining diacritical marks so that
/// accented characters match their ASCII base ("naïve" -> "naive").
/// Returns an empty set for strings shorter than 3 bytes.
fn trigrams(s: &str) -> HashSet<[u8; 3]> {
    let normalized: String = s
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(|c| c.to_lowercase())
        .collect();
    let bytes = normalized.as_bytes();
    if bytes.len() < 3 { return HashSet::new(); }
    bytes.windows(3)
        .map(|w| [w[0], w[1], w[2]])
        .collect()
}

/// Returns true if the character is a combining diacritical mark.
/// Covers the U+0300..U+036F block which handles all Latin-script
/// diacritics (acute, grave, circumflex, tilde, diaeresis, etc.).
fn is_combining_mark(c: char) -> bool {
    ('\u{0300}'..='\u{036F}').contains(&c)
}

/// Fraction of query trigrams found in the candidate text.
/// Asymmetric: measures how much of the query is *contained* in
/// the candidate, regardless of how much extra text the candidate has.
fn trigram_containment(query: &HashSet<[u8; 3]>, text: &HashSet<[u8; 3]>) -> f64 {
    if query.is_empty() { return 0.0; }
    let found = query.intersection(text).count();
    found as f64 / query.len() as f64
}

/// Computes a query-relevance score for a search result.
///
/// Splits the query into words and checks each word's trigram
/// containment independently against the full title and description
/// text. This avoids penalizing results that use compound forms
/// ("KDTree") or different word boundaries than the query ("kd tree").
///
/// Returns a value in [0.0, 1.0]. Queries where no word produces
/// trigrams (all words < 3 chars) return 1.0 so they don't
/// penalize results.
pub fn score_relevance(query: &str, title: &str, description: &str) -> f64 {
    // Generate trigrams per query word, skipping words too short
    // for trigrams (< 3 chars). Short words are effectively stop
    // words that appear everywhere and don't disambiguate.
    let word_trigrams: Vec<HashSet<[u8; 3]>> = query
        .split_whitespace()
        .map(|w| trigrams(w))
        .filter(|t| !t.is_empty())
        .collect();

    if word_trigrams.is_empty() { return 1.0; }

    // Trigram the full text (not per-word) so compound forms like
    // "KDTree" produce trigrams that span the original word boundary.
    let title_tri = trigrams(title);
    let desc_tri  = trigrams(description);

    // Per-word containment averaged across all query words.
    let mut total = 0.0;
    for wt in &word_trigrams {
        let title_cont = trigram_containment(wt, &title_tri);
        let desc_cont  = trigram_containment(wt, &desc_tri);
        total += 0.7 * title_cont + 0.3 * desc_cont;
    }

    (total / word_trigrams.len() as f64).clamp(0.0, 1.0)
}

// --- Rerank ---

/// Re-ranks search results using the three-layer scoring system.
/// Removes blocked domains and re-orders by combined score.
///
/// The final score for each result is:
///   base    = alpha * upstream_norm + beta * L1 + gamma * L2
///   penalty = (1 - delta) + delta * relevance
///   combined = base * penalty
///
/// where upstream_norm is the original score normalized to [0, 1],
/// L1 is the source identity score, L2 is the audience
/// sophistication score, and relevance is the trigram query-relevance
/// score. Delta controls penalty strength: 0.0 disables the penalty,
/// 1.0 makes the score fully proportional to relevance.
pub fn rerank(
    data    : &RerankData,
    config  : &RerankConfig,
    query   : &str,
    results : &mut Vec<SearchResult<EngineSearchResult>>,
) {
    if results.is_empty() {
        return;
    }

    // Find the maximum upstream score for normalization.
    let max_score = results
        .iter()
        .map(|r| r.score)
        .fold(0.0_f64, f64::max);

    if max_score == 0.0 {
        return;
    }

    // Compute L1 scores in a single pass. None means the domain
    // is blocked and the result should be removed.
    let l1_scores: Vec<Option<f64>> = results
        .iter()
        .map(|r| data.score_l1(&r.result.url, &config.url_signal_weights))
        .collect();

    // Filter out blocked results and update scores.
    let mut kept: Vec<(SearchResult<EngineSearchResult>, f64)> = Vec::new();
    for (result, l1_opt) in results.drain(..).zip(l1_scores.into_iter()) {
        let Some(l1) = l1_opt
        else { continue };

        kept.push((result, l1));
    }

    // Compute combined scores and write them back.
    for (result, l1) in &mut kept {
        let upstream_norm = result.score / max_score;
        let l2 = score_l2(&result.result.description, &config.l2_weights);
        let relevance = score_relevance(query, &result.result.title, &result.result.description);

        // Relevance acts as a multiplicative penalty on the base score.
        let base = config.alpha * upstream_norm
            + config.beta * *l1
            + config.gamma * l2;
        let penalty = (1.0 - config.delta) + config.delta * relevance;
        let combined = base * penalty;

        result.score = combined;
    }

    // Sort descending by combined score.
    kept.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Move results back into the original vec.
    *results = kept.into_iter().map(|(r, _)| r).collect();
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::config::{L2FeatureWeights, RerankConfig, UrlSignalWeights};
    use crate::engines::{EngineSearchResult, SearchResult};
    use super::{
        extract_domain, rerank, score_l2, score_relevance, trigram_containment,
        trigrams, RerankData,
    };

    /// Creates default L2 feature weights for tests.
    fn default_l2_weights() -> L2FeatureWeights {
        L2FeatureWeights::default()
    }

    /// Creates default URL signal weights for tests.
    fn default_url_weights() -> UrlSignalWeights {
        UrlSignalWeights::default()
    }

    /// Creates a minimal RerankData with explicit blocklist and reputation.
    fn make_rerank_data(
        blocklist  : Vec<&str>,
        reputation : Vec<(&str, f64)>,
    )
        -> RerankData
    {
        RerankData {
            blocklist:  blocklist.into_iter().map(|s| s.to_string()).collect(),
            reputation: reputation
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    /// Creates a search result with the given URL, title, description, and score.
    fn make_result(
        url         : &str,
        title       : &str,
        description : &str,
        score       : f64,
    )
        -> SearchResult<EngineSearchResult>
    {
        SearchResult {
            result: EngineSearchResult {
                url:         url.to_string(),
                title:       title.to_string(),
                description: description.to_string(),
            },
            engines: BTreeSet::new(),
            score:   score,
        }
    }

    /// Creates a default RerankConfig for tests with specified weights.
    fn make_config(alpha: f64, beta: f64, gamma: f64, delta: f64) -> RerankConfig {
        RerankConfig {
            enabled:            true,
            alpha:              alpha,
            beta:               beta,
            gamma:              gamma,
            delta:              delta,
            blocklist_path:     String::new(),
            reputation_path:    String::new(),
            l2_weights:         default_l2_weights(),
            url_signal_weights: default_url_weights(),
        }
    }

    // -- Layer 2 tests --

    #[test]
    fn l2_empty_snippet() {
        let score = score_l2("", &default_l2_weights());
        assert_eq!(score, 0.5, "empty snippet should return neutral 0.5");
    }

    #[test]
    fn l2_short_snippet() {
        let score = score_l2("hi there", &default_l2_weights());
        assert_eq!(score, 0.5, "short snippet should return neutral 0.5");
    }

    #[test]
    fn l2_technical_vs_marketing() {
        let weights = default_l2_weights();

        let technical = score_l2(
            "The TransactionManager implements asynchronous distributed \
             consensus via ConcurrencyControl mechanisms",
            &weights,
        );
        let marketing = score_l2(
            "Buy the best products at amazing prices today",
            &weights,
        );

        assert!(
            technical > marketing,
            "technical snippet ({technical}) should score higher \
             than marketing snippet ({marketing})",
        );
    }

    // -- Layer 1 tests --

    #[test]
    fn l1_blocked_domain() {
        let data = make_rerank_data(vec!["spam.com"], vec![]);
        let weights = default_url_weights();
        let result = data.score_l1("https://spam.com/page", &weights);
        assert!(result.is_none(), "blocked domain should return None");
    }

    #[test]
    fn l1_pinned_domain() {
        let data = make_rerank_data(vec![], vec![("docs.rs", 2.0)]);
        let weights = default_url_weights();
        let score = data
            .score_l1("https://docs.rs/serde/latest", &weights)
            .expect("pinned domain should not be blocked");

        // pin(2.0) -> modifier of +1.0, plus URL signals.
        assert!(
            score > 0.5,
            "pinned domain should have a high L1 score, got {score}",
        );
    }

    #[test]
    fn l1_unknown_domain() {
        let data = make_rerank_data(vec![], vec![]);
        let weights = default_url_weights();
        let score = data
            .score_l1("https://example.com/page", &weights)
            .expect("unknown domain should not be blocked");

        // Reputation modifier is 0.0. Score should be small, near zero
        // (just URL structure signals).
        assert!(
            score.abs() < 0.6,
            "unknown domain should have near-zero L1, got {score}",
        );
    }

    #[test]
    fn subdomain_walking() {
        let data = make_rerank_data(vec![], vec![("docs.rs", 2.0)]);
        let weight = data.get_domain_weight("foo.docs.rs");
        assert_eq!(
            weight, 2.0,
            "subdomain 'foo.docs.rs' should find 'docs.rs' entry",
        );
    }

    #[test]
    fn toml_overrides_blocklist() {
        let data = make_rerank_data(
            vec!["example.com"],
            vec![("example.com", 1.5)],
        );
        let weight = data.get_domain_weight("example.com");
        assert_eq!(
            weight, 1.5,
            "TOML reputation should override blocklist",
        );
    }

    // -- Full rerank tests --

    #[test]
    fn rerank_noop_preserves_order() {
        let data = make_rerank_data(vec![], vec![]);
        let config = make_config(1.0, 0.0, 0.0, 0.0);

        let mut results = vec![
            make_result("https://first.com/a", "first", "first result content here", 3.0),
            make_result("https://second.com/b", "second", "second result content here", 2.0),
            make_result("https://third.com/c", "third", "third result content here", 1.0),
        ];

        rerank(&data, &config, "test query", &mut results);

        assert_eq!(results.len(), 3);
        assert!(
            results[0].result.url.contains("first"),
            "with alpha=1.0 beta=0 gamma=0 delta=0, order should be preserved",
        );
        assert!(
            results[1].result.url.contains("second"),
            "with alpha=1.0 beta=0 gamma=0 delta=0, order should be preserved",
        );
        assert!(
            results[2].result.url.contains("third"),
            "with alpha=1.0 beta=0 gamma=0 delta=0, order should be preserved",
        );
    }

    #[test]
    fn pinned_domain_surfaces() {
        let data = make_rerank_data(vec![], vec![("docs.rs", 2.0)]);
        // Strong beta weight so reputation dominates.
        let config = make_config(0.1, 0.8, 0.1, 0.0);

        // The pinned domain starts at a much lower score.
        let mut results = vec![
            make_result(
                "https://random-site.com/page",
                "random site",
                "some generic content here now",
                10.0,
            ),
            make_result(
                "https://another-site.com/page",
                "another site",
                "another generic content here now",
                9.0,
            ),
            make_result(
                "https://docs.rs/serde/latest",
                "serde docs",
                "Serialization framework for Rust documentation",
                1.0,
            ),
        ];

        rerank(&data, &config, "serde", &mut results);

        assert_eq!(
            results[0].result.url, "https://docs.rs/serde/latest",
            "pinned domain should surface to the top",
        );
    }

    #[test]
    fn blocked_domains_excluded() {
        let data = make_rerank_data(vec!["spam.com", "junk.org"], vec![]);
        let config = make_config(1.0, 0.0, 0.0, 0.0);

        let mut results = vec![
            make_result("https://good.com/page", "good page", "good content right here now", 3.0),
            make_result("https://spam.com/page", "spam page", "spam content right here now", 2.0),
            make_result("https://junk.org/page", "junk page", "junk content right here now", 1.0),
        ];

        rerank(&data, &config, "test query", &mut results);

        assert_eq!(results.len(), 1, "blocked domains should be removed");
        assert!(
            results[0].result.url.contains("good"),
            "only the non-blocked result should remain",
        );
    }

    // -- Domain extraction tests --

    #[test]
    fn domain_extraction() {
        assert_eq!(
            extract_domain("https://www.example.com/path"),
            Some("example.com".to_string()),
        );
        assert_eq!(
            extract_domain("https://docs.rs/serde"),
            Some("docs.rs".to_string()),
        );
        assert_eq!(
            extract_domain("https://sub.domain.example.org/"),
            Some("sub.domain.example.org".to_string()),
        );
        assert_eq!(
            extract_domain("not a url"),
            None,
        );
    }

    // -- URL signal tests --

    #[test]
    fn url_signal_docs_path() {
        let data = make_rerank_data(vec![], vec![]);
        let weights = default_url_weights();

        let docs_score = data
            .score_l1("https://example.com/docs/api-reference", &weights)
            .unwrap();
        let plain_score = data
            .score_l1("https://example.com/about", &weights)
            .unwrap();

        assert!(
            docs_score > plain_score,
            "URL with /docs/ path ({docs_score}) should score higher \
             than plain URL ({plain_score})",
        );
    }

    // -- Trigram tests --

    #[test]
    fn trigrams_basic() {
        let tri = trigrams("abcd");
        assert_eq!(tri.len(), 2);
        assert!(tri.contains(&[b'a', b'b', b'c']));
        assert!(tri.contains(&[b'b', b'c', b'd']));
    }

    #[test]
    fn trigrams_short_string() {
        assert!(trigrams("ab").is_empty(), "strings < 3 bytes should produce no trigrams");
        assert!(trigrams("").is_empty());
    }

    #[test]
    fn trigrams_case_insensitive() {
        let upper = trigrams("ABC");
        let lower = trigrams("abc");
        assert_eq!(upper, lower, "trigrams should be case-insensitive");
    }

    #[test]
    fn trigrams_strip_diacritics() {
        let accented = trigrams("naïve");
        let plain    = trigrams("naive");
        assert_eq!(
            accented, plain,
            "diacritics should be stripped: naïve == naive",
        );
    }

    #[test]
    fn containment_full() {
        let query = trigrams("gigavoxels");
        // Text contains the query verbatim plus extra.
        let text = trigrams("gigavoxels rendering pipeline");
        let sim = trigram_containment(&query, &text);
        assert!(
            (sim - 1.0).abs() < f64::EPSILON,
            "all query trigrams present should give 1.0, got {sim}",
        );
    }

    #[test]
    fn containment_disjoint() {
        let query = trigrams("aaa bbb");
        let text = trigrams("xxx yyy");
        let sim = trigram_containment(&query, &text);
        assert!(
            sim < f64::EPSILON,
            "disjoint sets should give 0.0, got {sim}",
        );
    }

    #[test]
    fn containment_empty() {
        let query = trigrams("abc");
        let empty = trigrams("");
        assert_eq!(trigram_containment(&query, &empty), 0.0);
        assert_eq!(trigram_containment(&empty, &query), 0.0);
    }

    #[test]
    fn containment_partial() {
        // "gigapixel" shares gig, iga, xel with "gigavoxels" = 3 of 8.
        let query = trigrams("gigavoxels");
        let text = trigrams("gigapixel");
        let sim = trigram_containment(&query, &text);
        assert!(
            sim > 0.3 && sim < 0.5,
            "partial overlap should be between 0.3 and 0.5, got {sim}",
        );
    }

    // -- Relevance tests --

    #[test]
    fn relevance_exact_match() {
        let score = score_relevance("gigavoxels", "GigaVoxels", "rendering with gigavoxels");
        assert!(
            score > 0.5,
            "exact match should have high relevance, got {score}",
        );
    }

    #[test]
    fn relevance_typo_variant() {
        // "gigavoxel" vs "gigavoxels" differs by one trailing 's'.
        // After cubic sharpening the score is lower in absolute terms,
        // but still well above the noise floor for unrelated results.
        let score = score_relevance("gigavoxel", "Gigavoxels rendering", "real-time voxel rendering");
        let unrelated = score_relevance("gigavoxel", "Topaz Gigapixel AI", "photo upscaling");
        assert!(
            score > unrelated * 2.0,
            "typo variant ({score}) should score much higher than unrelated ({unrelated})",
        );
    }

    #[test]
    fn relevance_partial_prefix_overlap() {
        // "gigavoxels" and "gigapixel" share the "giga" prefix and
        // "xel" suffix, giving 3/8 query trigrams in the title.
        let score = score_relevance(
            "gigavoxels",
            "Topaz Gigapixel AI",
            "AI image upscaling software for photographers",
        );
        assert!(
            score < 0.35 && score > 0.15,
            "partial prefix overlap should score moderately low, got {score}",
        );
    }

    #[test]
    fn relevance_truly_unrelated() {
        let score = score_relevance(
            "gigavoxels",
            "Best Italian restaurants in Portland",
            "Find amazing pasta and pizza near downtown Portland",
        );
        assert!(
            score < 0.05,
            "completely unrelated result should score near zero, got {score}",
        );
    }

    #[test]
    fn relevance_multiword_compound_form() {
        // "kd tree" should match "KDTree" even though the word boundary
        // differs. "kd" is too short for trigrams and gets skipped, but
        // "tree" trigrams appear in "KDTree".
        let score = score_relevance(
            "kd tree",
            "KDTree — scikit-learn documentation",
            "Efficient nearest neighbor queries using a KD tree",
        );
        assert!(
            score > 0.7,
            "compound form should match well, got {score}",
        );
    }

    #[test]
    fn relevance_multiword_all_terms() {
        // Multi-word query: both substantive words should contribute.
        let both = score_relevance(
            "bevy ecs",
            "Introduction to Bevy ECS",
            "Entity component system for the Bevy game engine",
        );
        let one_missing = score_relevance(
            "bevy ecs",
            "Bevy Engine homepage",
            "A data-driven game engine built in Rust",
        );
        assert!(
            both > one_missing,
            "matching all query words ({both}) should outscore \
             matching only one ({one_missing})",
        );
    }

    #[test]
    fn relevance_short_words_skipped() {
        // A query of all short words should return 1.0 (no penalty).
        let score = score_relevance("I am a", "anything", "anything at all");
        assert_eq!(
            score, 1.0,
            "query of all short words should return 1.0 (no penalty)",
        );
    }

    #[test]
    fn relevance_short_query() {
        let score = score_relevance("ab", "anything", "anything at all");
        assert_eq!(
            score, 1.0,
            "very short query should return 1.0 (no penalty)",
        );
    }

    // -- Relevance integration test --

    #[test]
    fn relevance_demotes_unrelated_result() {
        let data = make_rerank_data(vec![], vec![]);
        // Upstream score provides the base, delta penalizes irrelevance.
        let config = make_config(1.0, 0.0, 0.0, 0.9);

        let mut results = vec![
            make_result(
                "https://topaz.com/gigapixel",
                "Topaz Gigapixel AI",
                "AI image upscaling software for photographers",
                10.0,
            ),
            make_result(
                "https://research.example.com/gigavoxels",
                "GigaVoxels: Ray-Guided Streaming",
                "A real-time rendering pipeline for detailed voxel scenes",
                5.0,
            ),
        ];

        rerank(&data, &config, "gigavoxels", &mut results);

        assert_eq!(
            results[0].result.url, "https://research.example.com/gigavoxels",
            "the relevant result should outrank the unrelated one \
             when delta dominates",
        );
    }
}
