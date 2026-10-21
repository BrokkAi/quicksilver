#!/usr/bin/env python3
"""Compare cargo-quicksilver and cargo-mutants verdicts on the mutants both generate.

Usage:
    compare_verdicts.py QS_REPORT.json CARGO_MUTANTS_OUTCOMES.json [--list]

QS_REPORT.json is the `--json` report of `cargo quicksilver run`.
CARGO_MUTANTS_OUTCOMES.json is `mutants.out/outcomes.json`.

Only function-body replacements (cargo-mutants genre FnValue) can match a QS
mutant. They are matched on (file, last line of the function, replacement
value): both tools report the closing-brace line the same way, while
cargo-mutants' start line includes doc comments and QS's does not.
"""

import collections
import json
import sys


def norm_value(text):
    text = text.strip()
    if text.startswith("{") and text.endswith("}"):
        text = text[1:-1].strip()
    if text == "":
        text = "()"
    return " ".join(text.split())


QS_KIND = {
    "caught": "caught",
    "missed": "missed",
    "not_covered": "not_covered",
    "unviable": "unviable",
    "errored": "errored",
    "timeout": "timeout",
}
CM_KIND = {
    "CaughtMutant": "caught",
    "MissedMutant": "missed",
    "Unviable": "unviable",
    "Timeout": "timeout",
    "Success": "missed",
}


def load_qs(path):
    out = {}
    for o in json.load(open(path))["outcomes"]:
        m = o["mutant"]
        key = (m["path"], m["end_line"], norm_value(m["replacement"]))
        out.setdefault(key, []).append((QS_KIND.get(o["kind"], o["kind"]), m["fqn"]))
    return out


def load_cm(path):
    out = {}
    genres = collections.Counter()
    for o in json.load(open(path))["outcomes"]:
        scenario = o.get("scenario")
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            continue
        m = scenario["Mutant"]
        genres[m.get("genre")] += 1
        if m.get("genre") != "FnValue":
            continue
        key = (m["file"], m["function"]["span"]["end"]["line"], norm_value(m["replacement"]))
        out.setdefault(key, []).append((CM_KIND.get(o["summary"], o["summary"]), m["name"]))
    return out, genres


def main():
    qs = load_qs(sys.argv[1])
    cm, genres = load_cm(sys.argv[2])
    show = "--list" in sys.argv
    both = sorted(set(qs) & set(cm))
    print(f"QS mutants: {sum(len(v) for v in qs.values())}")
    print(f"cargo-mutants mutants by genre: {dict(genres)}")
    print(f"matched (same file, function end line, replacement): {len(both)}")
    print(f"QS only: {len(set(qs) - set(cm))}; cargo-mutants FnValue only: {len(set(cm) - set(qs))}")
    cross = collections.Counter()
    rows = []
    for key in both:
        a = qs[key][0][0]
        b = cm[key][0][0]
        cross[(a, b)] += 1
        if a != b:
            rows.append((a, b, key, qs[key][0][1]))
    print("\nverdicts on matched mutants (QS, cargo-mutants): count")
    for (a, b), n in sorted(cross.items(), key=lambda x: -x[1]):
        mark = "" if a == b else "   <- differs"
        print(f"  {a:>12} {b:>12}: {n}{mark}")
    if show:
        print("\ndisagreements:")
        for a, b, key, fqn in sorted(rows):
            print(f"  QS={a:<12} cm={b:<12} {key[0]}:{key[1]} {fqn} -> {key[2]}")


if __name__ == "__main__":
    main()
