#!/usr/bin/env python3
"""Tests for the load-bearing parts of plan_review.py: shard arithmetic and
frontmatter parsing. Run with `python3 scripts/test_plan_review.py` — plain
asserts, no test-framework dependency, so it runs anywhere the skill is vendored.

The shard maths is critical: an off-by-one in the ranges would silently leave
files unreviewed (a coverage gap that looks like a clean pass), so the tiling
property below — every file in exactly one shard — is the test that matters most.
"""

import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import plan_review as pr  # noqa: E402


def test_shard_ranges_known_cases():
    # Even split, no remainder.
    assert pr.shard_ranges(75, 3) == [(1, 25), (26, 50), (51, 75)]
    assert pr.shard_ranges(100, 4) == [(1, 25), (26, 50), (51, 75), (76, 100)]
    # Remainder spread across the leading shards, not dumped on a straggler.
    assert pr.shard_ranges(95, 4) == [(1, 24), (25, 48), (49, 72), (73, 95)]
    # Single shard covers everything.
    assert pr.shard_ranges(17, 1) == [(1, 17)]
    # More shards than files: the trailing shards collapse to empty ranges
    # (start > end), which sed renders as no lines — harmless, never happens in
    # practice because plan_shards never returns count > file_set_size.
    assert pr.shard_ranges(2, 3) == [(1, 1), (2, 2), (3, 2)]


def test_shard_ranges_tile_exactly():
    """Every file lands in exactly one shard, with no gaps or overlaps."""
    for n in range(1, 200):
        for k in range(1, min(n, pr.MAX_SHARDS_PER_BRIEF) + 1):
            ranges = pr.shard_ranges(n, k)
            assert len(ranges) == k
            covered = []
            for start, end in ranges:
                covered.extend(range(start, end + 1))
            assert covered == list(range(1, n + 1)), (n, k, ranges)
            # Sizes differ by at most one — no straggler.
            sizes = [end - start + 1 for start, end in ranges]
            assert max(sizes) - min(sizes) <= 1, (n, k, sizes)


def test_plan_shards_threshold_and_cap():
    t = pr.SHARD_TARGET_SIZE
    # Not shardable (whole-tree) → always one reviewer, however large.
    assert pr.plan_shards(10_000, shardable=False) == 1
    # At or below the target size → one reviewer.
    assert pr.plan_shards(1, shardable=True) == 1
    assert pr.plan_shards(t, shardable=True) == 1
    # Just over the target → two reviewers.
    assert pr.plan_shards(t + 1, shardable=True) == 2
    # Scales with size…
    assert pr.plan_shards(t * 3, shardable=True) == 3
    # …until the cap, beyond which slices grow instead of agents.
    big = t * (pr.MAX_SHARDS_PER_BRIEF + 5)
    assert pr.plan_shards(big, shardable=True) == pr.MAX_SHARDS_PER_BRIEF


def test_build_shards_shapes():
    t = pr.SHARD_TARGET_SIZE
    # Unsharded diff brief: the explicit changed-file list rides inline.
    diff_small = pr.build_shards(2, shardable=True, extent="diff",
                                 target_files=["a.py", "b.py"])
    assert diff_small == [{"index": 1, "total": 1, "files": ["a.py", "b.py"]}]

    # Unsharded full brief: the reviewer enumerates the whole scope itself.
    full_small = pr.build_shards(5, shardable=True, extent="full", target_files=None)
    assert full_small == [{"index": 1, "total": 1, "whole": True}]

    # Whole-tree brief never shards, even with a large set, in either extent.
    whole_tree = pr.build_shards(t * 4, shardable=False, extent="full", target_files=None)
    assert whole_tree == [{"index": 1, "total": 1, "whole": True}]

    # Large per-file brief: range shards, no file list relayed.
    sharded = pr.build_shards(t * 3, shardable=True, extent="full", target_files=None)
    assert len(sharded) == 3
    assert all(set(s) == {"index", "total", "start", "end"} for s in sharded)
    assert [s["index"] for s in sharded] == [1, 2, 3]
    assert all(s["total"] == 3 for s in sharded)
    # The ranges still tile the whole set.
    covered = []
    for s in sharded:
        covered.extend(range(s["start"], s["end"] + 1))
    assert covered == list(range(1, t * 3 + 1))


def test_parse_frontmatter_strips_inline_comments():
    # A documented example copied verbatim, inline comments and all, still parses
    # — the comment must not leak into the compared value.
    path = _write_brief(
        "---\nextent: full   # or omit for the default, diff\n"
        "sweep: whole-tree   # or omit for the default, per-file\n---\nBody.\n"
    )
    assert pr.parse_frontmatter(path) == ("full", "whole-tree", True, [])
    os.unlink(path)


def _write_brief(body):
    handle = tempfile.NamedTemporaryFile(
        mode="w", suffix=".md", delete=False, encoding="utf-8"
    )
    handle.write(body)
    handle.close()
    return handle.name


def test_parse_frontmatter_defaults_and_values():
    # No frontmatter → both defaults, sweep not explicit, no warnings.
    path = _write_brief("Just prose, no frontmatter.\n")
    assert pr.parse_frontmatter(path) == ("diff", "per-file", False, [])
    os.unlink(path)

    # Both keys set explicitly → sweep_explicit is True.
    path = _write_brief("---\nextent: full\nsweep: whole-tree\n---\nBody.\n")
    assert pr.parse_frontmatter(path) == ("full", "whole-tree", True, [])
    os.unlink(path)

    # extent: full with the default (omitted) sweep stays per-file and not explicit
    # — this is exactly the case the sweep advisory nudges about.
    path = _write_brief("---\nextent: full\n---\nBody.\n")
    assert pr.parse_frontmatter(path) == ("full", "per-file", False, [])
    os.unlink(path)


def test_parse_frontmatter_unrecognised_values_warn():
    path = _write_brief("---\nextent: partial\nsweep: sideways\n---\nBody.\n")
    extent, sweep, sweep_explicit, warnings = pr.parse_frontmatter(path)
    # Bad values fall back to the defaults rather than breaking the run…
    assert (extent, sweep) == ("diff", "per-file")
    # …an unrecognised sweep does not count as explicitly declared…
    assert sweep_explicit is False
    # …and each is flagged so the author notices.
    assert len(warnings) == 2
    assert any("extent" in w for w in warnings)
    assert any("sweep" in w for w in warnings)
    os.unlink(path)


def main():
    tests = [
        test_shard_ranges_known_cases,
        test_shard_ranges_tile_exactly,
        test_plan_shards_threshold_and_cap,
        test_build_shards_shapes,
        test_parse_frontmatter_defaults_and_values,
        test_parse_frontmatter_unrecognised_values_warn,
        test_parse_frontmatter_strips_inline_comments,
    ]
    for test in tests:
        test()
        print(f"ok  {test.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    main()
