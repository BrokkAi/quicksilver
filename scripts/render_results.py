#!/usr/bin/env python3
"""Render the README results chart (requires matplotlib).

    python3 scripts/render_results.py --preview /tmp/quicksilver-results.png

Data comes from README.md's real-project results. Explanations of the review
populations and the grouped categories belong in the README, outside the image.
"""

import argparse
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


PROJECTS = (
    ("Small project", "mbx", "150k Rust lines · 7 crates", 40, (31, 0, 9)),
    ("Large project", "Bifrost", "3.3M Rust lines · ~30 crates", 440, (263, 32, 145)),
)
CATEGORIES = ("Test gaps", "Suspected bugs", "Other findings")
COLORS = ("#147d79", "#b96525", "#a6adb7")
INK = "#202b36"
SECONDARY = "#52606d"


def render(preview: Path | None) -> None:
    plt.rcParams.update({
        "font.family": "DejaVu Sans",
        "font.size": 17,
        "svg.fonttype": "path",
        "svg.hashsalt": "quicksilver-results",
    })
    fig = plt.figure(figsize=(15, 6.4), facecolor="white")
    for left, (size, name, scale, total, counts) in zip((0.035, 0.535), PROJECTS):
        assert sum(counts) == total
        fig.text(left, 0.93, size, fontsize=17, color=SECONDARY)
        fig.text(left, 0.85, name, fontsize=31, weight="bold", color=INK)
        fig.text(left, 0.785, scale, fontsize=16, color=SECONDARY)
        fig.text(left, 0.70, f"{total:,} mutants reviewed", fontsize=18, color=INK)

        ax = fig.add_axes((left + 0.15, 0.18, 0.265, 0.445), facecolor="white")
        shares = [100 * count / total for count in counts]
        ax.barh(range(3), shares, height=0.38, color=COLORS)
        ax.set_xlim(0, 100)
        ax.set_ylim(2.65, -0.65)
        ax.set_yticks(range(3), CATEGORIES, color=INK)
        ax.set_xticks((0, 50, 100), ("0%", "50%", "100%"), color=SECONDARY)
        ax.tick_params(axis="both", length=0, pad=12, labelsize=16)
        for spine in ax.spines.values():
            spine.set_visible(False)
        ax.spines["bottom"].set_visible(True)
        ax.spines["bottom"].set_color("#c9ced4")
        for row, (share, count) in enumerate(zip(shares, counts)):
            ax.text(share + 2.5, row, f"{count:,}", va="center", fontsize=19,
                    weight="bold" if row == 0 else "normal", color=INK)

    fig.text(0.5, 0.055, "Share of reviewed mutants", ha="center",
             fontsize=17, color=SECONDARY)
    output = Path(__file__).resolve().parents[1] / "assets" / "results.svg"
    output.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(output, metadata={"Date": None}, facecolor="white")
    # Matplotlib leaves spaces at the ends of SVG path lines.
    output.write_text("\n".join(line.rstrip() for line in output.read_text().splitlines()) + "\n")
    if preview:
        fig.savefig(preview, dpi=130, facecolor="white")
    plt.close(fig)
    print(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preview", type=Path, help="also render a PNG at this path")
    render(parser.parse_args().preview)
