# Search Result Re-Ranking Algorithm

## Specification for metasearch2 Integration

**Status:** Ready for implementation
**Based on:** 450-result quality experiment, Google/Yandex leak analysis, DOJ trial testimony
**Last updated:** 2026-02-19

---

## Design Rationale

This algorithm re-ranks results already scored by upstream search providers (Google, Bing, etc.). Those providers have already applied link-graph analysis, click-feedback loops (NavBoost and equivalents), neural relevance scoring, and site authority calculations at a scale we cannot replicate.

Our re-ranking addresses a specific gap: upstream providers optimize for the modal searcher. For technical and domain-expert queries, this produces results that are high-quality by general measures but wrong-audience — commercial content disguised as informational, mass-audience explainers, and SEO-optimized pages targeting transactional intent.

The algorithm applies two deterministic scoring layers that are cheap to compute (<1µs per result), require no ML inference, no GPU, no model loading, and no user telemetry. Both layers target axes that upstream providers under-optimize for: source identity and audience sophistication.

### Why No Click/Behavioral Tracking

Click tracking was evaluated and rejected for four reasons:

1. **Upstream already consumed it.** Results arrive pre-ranked by algorithms that already incorporated click signals. Re-applying click-based scoring double-counts a noisy axis with less information than the upstream provider had.

2. **Clicks are an adversarial attack surface, not a quality signal.** Clickbait exploits prediction errors in human attention. This isn't a sophistication gradient — it's a psychological trap that works on experts. Any behavioral metric we expose becomes an optimization target for content producers.

3. **Feedback loops at single-user scale.** Position bias (higher-ranked results get more clicks) is correctable at Google's volume but pathological for a single user or small population. We'd be training on our own biased behavior.

4. **The signal is redundant.** "This domain is good" is captured by Layer 1. "This content targets experts" is captured by Layer 2. The marginal information from behavioral tracking is small and comes with all the above costs.

Explicit feedback (manual domain pinning/blocking) is retained as the only user-behavior input, since it's intentional, has no feedback loops, and updates the deterministic scoring directly.

---

## Architecture Overview

```
Upstream Results (30 per query)
        │
        ▼
┌─────────────────────┐
│  Layer 1: Source     │  Domain reputation lookup
│  Identity            │  ~0.5µs per result
│                      │  WHO is publishing this?
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  Layer 2: Audience   │  Structural feature scoring
│  Sophistication      │  <0.5µs per result
│                      │  WHO is this written for?
└─────────┬───────────┘
          │
          ▼
    Score Combination
          │
          ▼
    Re-ranked Results
```

Total latency budget: <1µs per result, <30µs for a full 30-result page.

---

## Layer 1: Source Identity

### Purpose

Determine WHAT KIND of source published this result. Separates knowledge sources from commercial entities, spam from legitimate content. This is the coarse filter — high recall, moderate precision.

### Components

#### 1a. Domain Blocklist

A static list of domains to exclude or heavily penalize.

- **Source:** Curated blocklist (~141K domains), supplemented by user blocks
- **Lookup:** Hash set, O(1)
- **Action:** Blocked domains receive a score of 0 (excluded from results)

#### 1b. Domain Reputation Weights

Explicit per-domain scoring adjustments in three tiers:

| Tier | Score Modifier | Description |
|------|---------------|-------------|
| Pinned | +1.0 | Known-excellent sources. Always surface. |
| Raised | +0.5 | Generally good sources. Boost over neutral. |
| Neutral | 0.0 | Default. No adjustment. |
| Lowered | -0.5 | Known-poor sources. Penalize but don't exclude. |
| Blocked | exclude | Never show. |

- **Source:** `domain_reputation.toml` (user-maintained)
- **Lookup:** Hash map, O(1)
- **Update mechanism:** Manual editing or explicit thumbs-up/down in search UI

#### 1c. URL Structure Signals

Heuristic signals derived from the URL itself, no page content needed:

| Signal | Computation | Rationale |
|--------|-------------|-----------|
| `url_depth` | Count of `/` separators in path | Deeper paths often indicate specific content vs. landing pages |
| `url_slug_word_count` | Words in final path segment | Descriptive slugs correlate with article/doc pages |
| `is_docs_path` | Path contains `/docs/`, `/api/`, `/reference/`, `/manual/`, `/wiki/`, `/spec/` | Documentation paths are strong positive signals |
| `is_forum_path` | Path contains `/forum/`, `/discuss/`, `/community/`, `/questions/`, `/answers/` | Forum/Q&A paths indicate community knowledge |
| `is_institutional_tld` | TLD is `.edu`, `.gov`, `.org`, `.mil` | Institutional domains have different incentive structures |
| `domain_token_count` | Hyphenated/dotted segments in domain | Spam domains tend to have more tokens |
| `has_subdomain` | Non-www subdomain present | Subdomains like `docs.x.com`, `wiki.x.com` are often positive |
| `is_commercial_pattern` | Domain matches known commercial patterns (see below) | Identifies likely commercial entities from URL alone |

**Commercial domain patterns** (regex-based):

```
# Equipment/product vendors
(store|shop|buy|order|supply|supplies|mart|outlet|deals|pricing)

# Service providers (legal, medical, consulting)
(law|legal|attorney|lawyer|consult|clinic|therapy|advisor)

# Marketing/SEO
(seo|marketing|agency|growth|leadgen|funnel)
```

These don't trigger blocking — they contribute a negative modifier that can be overridden by strong Layer 2 scores or explicit reputation pinning. A legal blog with excellent technical content should still be discoverable.

### Layer 1 Score

```
L1(result) =
    if domain in blocklist: EXCLUDE
    else: reputation_weight(domain) + url_signal_score(url)
```

Where `url_signal_score` is a weighted sum of the URL structure signals, normalized to [-0.5, +0.5].

---

## Layer 2: Audience Sophistication

### Purpose

Determine WHO the content targets. Separates expert-oriented content from mass-audience content. This is the fine-grained discriminator — operates within the results that pass Layer 1.

### Key Experimental Finding

The top 5 most predictive features from the 450-result experiment were all vocabulary sophistication proxies. These features were anti-correlated with general prose quality (as measured by the NVIDIA quality-classifier-deberta), confirming they measure a genuinely different axis. Dense technical writing scores LOW on readable-prose metrics and HIGH on these features.

### Features

All computed from the search result snippet text (typically 50-150 characters). No page fetch required.

| Feature | Computation | Signal |
|---------|-------------|--------|
| `capitalized_word_ratio` | (Words starting uppercase, excluding sentence-initial) / total words | Technical terms, acronyms, proper nouns. Higher = more specialized |
| `unique_word_ratio` | Unique words / total words | Lexical diversity. Technical content uses varied vocabulary; marketing repeats keywords |
| `technical_term_density` | Words ≥ 8 characters / total words | Long words correlate with domain-specific terminology |
| `avg_word_length` | Mean character count per word | Technical/academic writing uses longer words on average |
| `char_per_word` | Total characters / total words | Similar to above but includes hyphenated compounds as single tokens |

### Feature Normalization

Features are normalized to [0, 1] using empirically-derived ranges from the 450-result dataset:

| Feature | Observed Min | Observed Max | Notes |
|---------|-------------|-------------|-------|
| `capitalized_word_ratio` | 0.0 | 0.45 | Clip outliers above 0.5 |
| `unique_word_ratio` | 0.40 | 1.0 | Very short snippets may hit 1.0 |
| `technical_term_density` | 0.0 | 0.50 | |
| `avg_word_length` | 3.5 | 8.0 | |
| `char_per_word` | 3.5 | 9.0 | |

### Layer 2 Score

```
L2(result) = weighted_sum(normalized_features)
```

Default weights (equal weighting as starting point — can be tuned):

```
capitalized_word_ratio:  0.20
unique_word_ratio:       0.20
technical_term_density:  0.20
avg_word_length:         0.20
char_per_word:           0.20
```

Normalized to [0, 1] range.

### Known Limitations

- Snippet text is short. Feature distributions are noisy for very short snippets (<30 characters).
- Non-English content may have different baseline word lengths. Current calibration is English-only.
- Some high-quality results have generic snippets (e.g., GitHub repos where the snippet is the repo description, not code). URL signals from Layer 1 partially compensate.

---

## Score Combination

### Final Score

```
S(result) = α * upstream_rank_score + β * L1(result) + γ * L2(result)
```

Where:

- `upstream_rank_score` is the inverse-rank position from the search provider, normalized to [0, 1]. Position 1 → 1.0, position 30 → 0.0 (linear interpolation). This preserves the upstream provider's relevance judgment as a baseline.
- `α`, `β`, `γ` control how aggressively we re-rank vs. deferring to upstream.

### Default Weights

```
α = 0.50   # Upstream relevance (we still trust their topical matching)
β = 0.30   # Source identity (coarse quality filter)
γ = 0.20   # Audience sophistication (fine-grained preference)
```

These weights mean:
- A result at position 1 with neutral domain reputation and average vocabulary gets: `0.50 * 1.0 + 0.30 * 0.0 + 0.20 * 0.5 = 0.60`
- A result at position 15 from a pinned domain with high vocabulary sophistication gets: `0.50 * 0.52 + 0.30 * 1.0 + 0.20 * 0.9 = 0.74`
- The pinned expert source at position 15 outranks the neutral source at position 1.

### Exclusion Rules

Results are excluded entirely (not scored) if:
- Domain is in the blocklist
- Domain is in the "blocked" reputation tier

All other results receive a score and are re-ranked.

---

## Explicit Feedback Mechanism

The only user-behavior input. No implicit tracking.

### Interface

Two actions available per search result:

- **Pin** (👍): Adds domain to `raised` tier (or `pinned` if already raised)
- **Block** (👎): Adds domain to `lowered` tier (or `blocked` if already lowered)

### Escalation Ladder

Each action moves the domain one step along:

```
pinned ← raised ← neutral → lowered → blocked
```

Multiple positive signals on the same domain escalate it. Same for negative.

### Storage

All feedback writes to `domain_reputation.toml`. No separate telemetry database, no behavioral logs, no session tracking.

---

## Implementation Notes

### Data Requirements

| File | Purpose | Size | Update Frequency |
|------|---------|------|-----------------|
| `blocked_domains_slim.txt` | Domain blocklist | ~3MB (141K domains) | Monthly / as-needed |
| `domain_reputation.toml` | Reputation weights + pins/blocks | <100KB | Per user action |

Both loaded into memory at startup. Hot-reloadable on file change.

### Computational Cost

| Operation | Cost | Notes |
|-----------|------|-------|
| Blocklist lookup | O(1) hash | ~0.1µs |
| Reputation lookup | O(1) hash | ~0.1µs |
| URL parsing + signals | String ops | ~0.2µs |
| Snippet tokenization | Split + count | ~0.1µs |
| Feature computation | 5 arithmetic ops | ~0.05µs |
| Score combination | 3 multiplies + 2 adds | ~0.01µs |
| **Total per result** | | **<0.5µs** |
| **Total per 30-result page** | | **<15µs** |

No network calls. No disk I/O after startup. No model inference.

### Integration Point

The re-ranker operates on the merged result list from all upstream providers, after deduplication but before display. Input is a list of `(url, snippet, upstream_rank)` tuples. Output is the same list, re-ordered by combined score.

```
fn rerank(results: &mut [(Url, Snippet, f32)], config: &RerankConfig) {
    results.retain(|(url, _, _)| !config.blocklist.contains(url.domain()));

    for (url, snippet, upstream_rank) in results.iter_mut() {
        let l1 = score_source_identity(url, &config.reputation, &config.url_patterns);
        let l2 = score_audience_sophistication(snippet, &config.feature_ranges);
        let combined = config.alpha * normalize_rank(*upstream_rank)
                     + config.beta * l1
                     + config.gamma * l2;
        *upstream_rank = combined;
    }

    results.sort_by(|(_, _, a), (_, _, b)| b.partial_cmp(a).unwrap());
}
```

### Tuning Strategy

The `α`, `β`, `γ` weights and Layer 2 feature weights are exposed as configuration. Initial values are best guesses from the experiment. Tuning approach:

1. Run metasearch2 with default weights for a few weeks of normal use
2. Periodically sample 30-result pages and manually rate top-10 vs. bottom-10
3. If too many good results are being buried: reduce `β` and `γ`, increase `α`
4. If too much noise is surfacing: increase `β` and `γ`, reduce `α`
5. Adjust individual feature weights if specific failure modes emerge (e.g., if `capitalized_word_ratio` is boosting ALL-CAPS spam)

No automated tuning. Manual adjustment based on observed behavior.

---

## Validation Criteria

This system is worth deploying if it measurably improves result relevance over raw upstream ranking. The bar is deliberately low:

- **Minimum viable improvement:** Top-10 re-ranked results contain fewer commercial/mass-audience results than top-10 upstream, across a sample of 20+ queries spanning known and unfamiliar domains.
- **No regressions:** Re-ranking should not bury results that were correctly highly-ranked by upstream. Specifically, authoritative primary sources (official docs, specs, RFCs) that appear in top-5 upstream should remain in top-10 after re-ranking.
- **Robustness:** The algorithm should not catastrophically fail on any query category. Unfamiliar domains should not be systematically penalized (this was a failure mode in the initial embedding experiment).

### What This Won't Do

- It won't match Google's relevance for the average user. It's not trying to.
- It won't find results that upstream providers didn't return. We're re-ranking, not discovering.
- It won't adapt to new domains automatically. Unknown domains get neutral scores until explicitly rated or until the blocklist/reputation file is updated.
- It won't work well for non-technical queries. The vocabulary sophistication features are calibrated for technical/domain-expert content. Recipe searches, travel planning, etc. would need different feature weights or a query-type classifier.

---

## Appendix A: Experimental Evidence

### Experiment Design
- 15 queries × 30 results = 450 manually rated results
- 3 domain-familiarity buckets: known (5 queries), adjacent (5), unfamiliar (5)
- Binary quality labels: high (useful for domain expert) / low (not useful)
- Base rate: 60.3% low, 39.7% high

### Key Findings

| Test | Result | Implication |
|------|--------|-------------|
| MiniLM embeddings | 60.6% accuracy (= baseline) | Semantic similarity models encode topic, not quality |
| UMAP clustering | Clustered by query, not quality | Quality is not a direction in embedding space |
| NVIDIA quality-classifier-deberta | rho = -0.100 (anti-correlated) | General prose quality opposes expert-content quality |
| FineWeb-edu classifier | rho = 0.070 (not significant) | Educational quality scores don't transfer to snippets |
| Snippet length | max |rho| = 0.097 | No signal |
| Intent regex (commercial/persuasive) | 50.7% accuracy (= majority baseline) | Commercial content has learned to not look commercial in snippets |
| Structural features (URL only) | 60.0% within-domain, 52.3% cross-domain | Domain/URL features carry real signal |
| Structural features (text only) | 55.0% within-domain, 48.4% cross-domain | Vocabulary features weaker alone but aid transfer |
| Structural features (combined) | 61.1% within-domain, **57.8% cross-domain** | Best result. Combined features beat either alone on transfer |

### Feature Importance (Random Forest)

Top 5 features by importance were all vocabulary sophistication proxies:
1. `capitalized_word_ratio`
2. `unique_word_ratio`
3. `technical_term_density`
4. `char_per_word`
5. `avg_word_length`

URL features ranked #6-7. Junk-signal features (marketing words, listicles, ad language, question bait) did not reach top 10.

## Appendix B: Corroboration from Search Engine Internals

### Sources
- Google Content Warehouse API leak (May 2024): 2,596 modules, 14,014 attributes
- Yandex source code leak (Jan 2023): ~17,800 ranking factors, actual code with coefficients
- DOJ v. Google antitrust trial testimony (2023-2024): Engineers under oath

### Key Corroborating Findings

| Our Finding | Google/Yandex Equivalent |
|-------------|-------------------------|
| Domain reputation is the strongest coarse signal | `siteAuthority`, `Q*` (query-independent quality score), `NSR` |
| Business model classification (commercial vs. informational) | Google explicitly classifies: news, YMYL, personal blogs, e-commerce, video |
| Vocabulary sophistication as quality proxy | `chardScores` (content quality), `contentEffort` (LLM-estimated effort), `gibberishScores` |
| Click tracking degrades quality without quality floors | DOJ testimony: "people tend to click on lower-quality, less-authoritative content" |
| Quality floors gate what behavioral signals can boost | `Q*` is mostly static, query-independent; NavBoost operates within Q* constraints |
| Site topical focus as positive signal | `siteFocusScore`, `siteRadius`, `siteEmbeddings` |
