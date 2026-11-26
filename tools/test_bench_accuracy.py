import unittest

import bench_accuracy as accuracy
import bench_shared as legacy


class AccuracyTests(unittest.TestCase):
    def test_frozen_suites_and_release_overlap(self):
        for suite, n, n_settled in (("quick", 300, 24), ("nightly", 2000, 2000), ("release", 2000, 2000)):
            with self.subTest(suite=suite):
                cases, _ = accuracy.choose(suite)
                self.assertEqual(n, len({c.intent for c, _ in cases}))
                self.assertEqual(n_settled, sum(enabled for _, enabled in cases))
                self.assertEqual(accuracy.MANIFESTS[suite], accuracy.sha(accuracy.payload(cases).encode()))
                if suite == "release":
                    overlaps = accuracy.overlap_annotations()
                    self.assertTrue(any("same_gold_context" in kinds for kinds in overlaps.values()))
                    for case, _ in cases:
                        self.assertFalse(overlaps.get(case.ref, {}).keys() - {"same_key_gold_only"})

    def test_numerical_interval_keeps_uncertainty_when_there_are_no_flips(self):
        lo, hi = accuracy.paired_interval(600, 0, 0, 0.025)
        self.assertAlmostEqual(-0.00842295, lo, places=7)
        self.assertAlmostEqual(-lo, hi)
        self.assertAlmostEqual(0.98304757249, accuracy.binomial_upper(3, 2, 0.05), places=8)

    def test_p5_only_regression_fails_quick_gate(self):
        cases = [(accuracy.Case("fixture", i, "word-full-writing", "gov", "a", "阿", str(i)), i < 24)
                 for i in range(300)]
        base = {}
        head = {}
        for i, (case, enabled) in enumerate(cases):
            ranks = {field: (3 if enabled else 0) for field in accuracy.RANKS}
            if not enabled:
                ranks["rank_settled"] = ranks["strict_rank_settled"] = -2
            base[case.ref] = dict(ranks, picks=1, pages=0)
            head[case.ref] = base[case.ref].copy()
            if i < 24:
                head[case.ref]["rank_8m"] = head[case.ref]["strict_rank_8m"] = 8
        expected = {case.ref: (case, enabled) for case, enabled in cases}
        accuracy.validate(base, expected)
        accuracy.validate(head, expected)
        lines, failures = accuracy.compare("quick", cases, base, head)
        self.assertEqual(1, len(failures))
        self.assertIn("8m/all P@5: -8.00%", failures[0])
        self.assertIn("| 300 | 300 | 276 | 0 | 24 | -8.00 ", "\n".join(lines))
        del head[next(iter(head))]
        with self.assertRaisesRegex(ValueError, "missing 1"):
            accuracy.validate(head, expected)

    def test_legacy_pairing_uses_ids_and_does_not_take_intersection(self):
        repeat = {"category": "daily", "input": "ni", "expected": "你", "rank": "0"}
        base = [dict(repeat, case_ref="a"), dict(repeat, case_ref="b", rank="5")]
        head = [dict(repeat, case_ref="b", rank="3"), dict(repeat, case_ref="a", rank="5")]
        pairs = legacy.paired(base, head)["daily"]
        self.assertEqual((0, 1), (pairs["top1"]["improved"], pairs["top1"]["regressed"]))
        self.assertEqual((1, 1), (pairs["top5"]["improved"], pairs["top5"]["regressed"]))
        with self.assertRaisesRegex(SystemExit, "用例不一致"):
            legacy.paired(base, head[:1])
        with self.assertRaisesRegex(SystemExit, "重复键"):
            legacy.paired([repeat, repeat], [repeat, repeat])
        with self.assertRaisesRegex(SystemExit, "字段不一致"):
            legacy.paired(base, [dict(head[0], expected="泥"), head[1]])

    def test_inconclusive_interval_stops_release_but_not_nightly(self):
        cases = [(accuracy.Case("fixture", i, "word-full-writing", "gov", "a", "阿", str(i)), True)
                 for i in range(100)]
        scores = {case.ref: dict(dict.fromkeys(accuracy.RANKS, 0), picks=1, pages=0) for case, _ in cases}
        nightly, failures = accuracy.compare("nightly", cases, scores, scores)
        self.assertFalse(failures)
        self.assertIn("settled/all P@1: inconclusive at -2 pp", "\n".join(nightly))
        _, failures = accuracy.compare("release", cases, scores, scores)
        self.assertEqual(["settled/all P@1: inconclusive at -2 pp",
                          "settled/clean P@1: inconclusive at -2 pp"], failures)


if __name__ == "__main__":
    unittest.main()
