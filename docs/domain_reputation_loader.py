#!/usr/bin/env python3
"""
Domain Reputation Loader
========================
Loads domain reputation data from two sources:
  1. blocked_domains.txt  - 402K community-curated blocked domains (weight: 0.0)
  2. domain_reputation.toml - hand-curated raised/pinned/lowered domains with weights

TOML entries always override the blocklist. This means if a domain appears in both
blocked_domains.txt and domain_reputation.toml, the TOML weight wins.

Usage:
    loader = DomainReputation("blocked_domains.txt", "domain_reputation.toml")
    weight = loader.get_weight("pinterest.com")   # 0.0 (blocked)
    weight = loader.get_weight("docs.rs")          # 2.0 (pinned)
    weight = loader.get_weight("unknown-site.org") # 1.0 (neutral default)
"""

import tomllib
from pathlib import Path


class DomainReputation:
    def __init__(self, blocklist_path: str, toml_path: str):
        self.weights: dict[str, float] = {}
        self._load_blocklist(blocklist_path)
        self._load_toml(toml_path)  # TOML loads second, overrides blocklist

    def _load_blocklist(self, path: str):
        """Load flat domain list, all get weight 0.0"""
        with open(path) as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#"):
                    self.weights[line] = 0.0

    def _load_toml(self, path: str):
        """Load TOML config, overriding any blocklist entries.
        
        TOML structure is nested: [action.category] -> {domains: [...]}
        e.g. [pin.rust] -> {domains: ["docs.rs", ...]}
        tomllib parses this as: {"pin": {"rust": {"domains": [...]}}}
        """
        weight_map = {"pin": 2.0, "raise": 1.5, "lower": 0.3, "block": 0.0, "neutral": 1.0}

        with open(path, "rb") as f:
            config = tomllib.load(f)

        for action, categories in config.items():
            weight = weight_map.get(action)
            if weight is None:
                continue
            if not isinstance(categories, dict):
                continue
            for category_name, section in categories.items():
                if not isinstance(section, dict):
                    continue
                domains = section.get("domains", [])
                for domain in domains:
                    self.weights[domain] = weight

    def get_weight(self, url_or_domain: str) -> float:
        """
        Look up domain weight. Tries exact match first, then walks up subdomains.
        Returns 1.0 (neutral) for unknown domains.

        Examples:
            "docs.rs" -> exact match -> 2.0
            "blog.fasterthanli.me" -> no exact match -> tries "fasterthanli.me" -> 2.0
            "random-site.com" -> no match -> 1.0
        """
        # Strip protocol/path if accidentally passed
        domain = url_or_domain
        if "://" in domain:
            domain = domain.split("://", 1)[1]
        domain = domain.split("/", 1)[0].split("?", 1)[0].lower()

        # Walk up the domain hierarchy
        parts = domain.split(".")
        for i in range(len(parts)):
            candidate = ".".join(parts[i:])
            if candidate in self.weights:
                return self.weights[candidate]

        return 1.0  # neutral default


def print_stats(loader: DomainReputation):
    """Print summary statistics"""
    from collections import Counter
    weight_names = {0.0: "blocked", 0.3: "lowered", 1.0: "neutral", 1.5: "raised", 2.0: "pinned"}
    counts = Counter(loader.weights.values())
    print("Domain Reputation Stats:")
    for weight in sorted(counts.keys()):
        name = weight_names.get(weight, f"weight={weight}")
        print(f"  {name:>10}: {counts[weight]:>7,} domains")
    print(f"  {'TOTAL':>10}: {sum(counts.values()):>7,} domains")


if __name__ == "__main__":
    loader = DomainReputation("blocked_domains.txt", "domain_reputation.toml")
    print_stats(loader)

    # Quick smoke test
    tests = [
        ("pinterest.com", 0.0),
        ("docs.rs", 2.0),
        ("fasterthanli.me", 2.0),
        ("en.cppreference.com", 2.0),
        ("medium.com", 0.3),
        ("howtogeek.com", 0.3),
        ("random-unknown-site.org", 1.0),
        ("developer.mozilla.org", 2.0),
        ("news.ycombinator.com", 1.5),
    ]
    print("\nSmoke test:")
    for domain, expected in tests:
        actual = loader.get_weight(domain)
        status = "✓" if actual == expected else "✗"
        print(f"  {status} {domain:40s} expected={expected} actual={actual}")
