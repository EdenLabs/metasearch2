# Claude Code: metasearch2 Re-Ranking Implementation

## Context

You are adding a search result re-ranking system to metasearch2, a metasearch engine. The system applies two deterministic scoring layers on top of upstream search provider results to filter noise and surface expert-oriented content.

This is based on a 450-result experiment that tested embeddings, pretrained quality classifiers, intent classification, and structural features. Most approaches failed. What worked: URL/domain features and vocabulary sophistication features computed from snippet text. The full experimental findings and architecture rationale are in `RERANKING_SPEC.md` in this repo — read it first, especially the Design Rationale and Architecture Overview sections.

## What We're Building

A post-deduplication re-ranking pass with two layers:

**Layer 1 — Source Identity (~0.5µs/result)**
- Domain blocklist lookup (hash set, ~141K domains from `blocked_domains_slim.txt`)
- Domain reputation weights (hash map from `domain_reputation.toml`)
- URL structure heuristics (path depth, slug analysis, docs/forum path detection, institutional TLD, commercial patterns)

**Layer 2 — Audience Sophistication (<0.5µs/result)**
- 5 features computed from snippet text: `capitalized_word_ratio`, `unique_word_ratio`, `technical_term_density`, `avg_word_length`, `char_per_word`
- These are vocabulary sophistication proxies — they were the top 5 features by importance in the experiment
- They are anti-correlated with general prose quality (expert content scores LOW on readability, HIGH on these)

**Score combination:**
```
S = α * upstream_rank_score + β * L1_score + γ * L2_score
```
Default weights: α=0.50, β=0.30, γ=0.20. Exposed as configuration.

**Explicit feedback:**
- Per-result pin (👍) / block (👎) that writes to `domain_reputation.toml`
- Escalation: neutral → raised → pinned, neutral → lowered → blocked
- No click tracking. No implicit behavioral signals. This was a deliberate architectural decision — see spec for rationale.

## Key Files Already in Repo

- `RERANKING_SPEC.md` — Full architecture spec with experimental evidence, feature definitions, normalization ranges, scoring formulas, and validation criteria. This is the source of truth.
- `domain_reputation.toml` — Existing domain reputation data (pinned/raised/lowered domains, weights)
- `blocked_domains_slim.txt` — Blocklist (~141K domains, one per line)
- `domain_reputation_loader.py` — Reference implementation for loading the reputation data (Python, for reference — the actual implementation will be in whatever language metasearch2 uses)

## Implementation Approach

1. **Read the codebase first.** Understand metasearch2's existing result pipeline — where results come in from upstream providers, where deduplication happens, how results are stored and rendered. The re-ranker slots in after deduplication, before display.

2. **Read `RERANKING_SPEC.md`** for the full architecture, scoring formulas, feature definitions, and normalization ranges.

3. **Implement Layer 1 and Layer 2 as a single re-ranking pass.** Don't over-engineer the module boundaries — both layers are simple enough to be one function that takes `(url, snippet, upstream_rank)` and returns a combined score.

4. **Load blocklist and reputation data at startup, keep in memory.** Both files are small enough to hold entirely in RAM. Support hot-reload on file change if the framework makes that easy, otherwise reload on restart is fine.

5. **Make weights configurable.** The α/β/γ combination weights and the per-feature weights in Layer 2 should all be in a config file, not hardcoded. We'll tune these based on real usage.

6. **Wire up the explicit feedback mechanism.** Pin/block actions from the UI should write to `domain_reputation.toml`. The escalation ladder is: pinned ↔ raised ↔ neutral ↔ lowered ↔ blocked. If there's no UI affordance for per-result actions yet, add minimal thumbs-up/thumbs-down buttons.

## Constraints

- **No ML models, no GPU inference, no model loading.** The whole point is that this runs in <1µs per result with pure arithmetic and hash lookups.
- **No click tracking, no dwell time, no implicit behavioral signals.** This was deliberately excluded — it's not a TODO, it's a design decision. See the spec's "Why No Click/Behavioral Tracking" section if you want the reasoning.
- **No network calls during scoring.** All data is local. The re-ranker must not add latency beyond the arithmetic.
- **Don't fetch pages.** All features are computed from the URL and the snippet text that upstream already returned. No additional HTTP requests.
- **Preserve upstream relevance as a signal.** We're re-ranking, not replacing. The upstream provider's topical matching is still valuable — that's what the α weight on `upstream_rank_score` preserves. A result at position 1 with neutral domain reputation should still rank highly.

## What NOT to Implement

- Query-type classification (technical vs. casual). The features are calibrated for technical content. If we later want to support non-technical queries, that's a separate feature flag, not a classifier.
- Site-focus scoring (how topically coherent a domain is). Good idea in theory but requires a domain metadata cache we don't have.
- Any form of personalization beyond explicit domain reputation edits.
- Caching of scores across queries. Results are cheap enough to score fresh every time.

## Testing Strategy

- Unit test the feature computation functions with known inputs and expected outputs. The spec has normalization ranges that imply expected distributions.
- Integration test: feed a fixed set of (url, snippet, rank) triples through the re-ranker and assert the output ordering changes in expected ways (e.g., a pinned domain at rank 20 should surface above a neutral domain at rank 5).
- Sanity check: the re-ranker should be a no-op (preserve upstream ordering) when β=0 and γ=0. Verify this.
- The spec's Validation Criteria section describes the acceptance bar for real-world evaluation.

## Questions You Should Ask (of me, not the codebase)

If anything is ambiguous about the scoring math, feature definitions, or architectural decisions, ask rather than guessing. The spec is detailed but the weight tuning and normalization ranges are educated guesses from the experiment, not gospel. Implementation details like data structures, error handling, and config format are your call — optimize for the existing codebase's conventions.
