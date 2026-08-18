#!/usr/bin/env python3
# Draw the benchmark charts from results.csv.  Run:  python3 plot.py
# Colours: grey = direct (no tunnel), teal = NatForge, orange = frp.
import csv

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

COLOR = {
    "direct": "#888888",
    "natforge": "#0d9488",
    "frp": "#ea580c",
}
LABEL = {
    "direct": "direct",
    "natforge": "NatForge",
    "frp": "frp",
}

# Load every measured row from the results file.
with open("results.csv") as results_file:
    rows = list(csv.DictReader(results_file))


def rows_for(test):
    # Return only the rows belonging to one test (latency / throughput / memory).
    selected = []
    for row in rows:
        if row["test"] == test:
            selected.append(row)
    return selected


def save(name):
    plt.savefig(name + ".png", dpi=120, bbox_inches="tight")
    plt.close()


# ---------------------------------------------------------------------------
# Figure 1: latency vs concurrency. Solid line = median (p50), dashed = tail (p99).
# ---------------------------------------------------------------------------
latency_rows = rows_for("latency")

# The distinct concurrency levels, in order, for the x axis.
connection_levels = set()
for row in latency_rows:
    connection_levels.add(int(row["conns"]))
connection_levels = sorted(connection_levels)

plt.figure(figsize=(7, 4.5))
for system in ["direct", "natforge", "frp"]:
    median_series = []
    tail_series = []
    for row in latency_rows:
        if row["system"] != system:
            continue
        median_series.append(float(row["p50_ms"]))
        tail_series.append(float(row["p99_ms"]))
    x_positions = range(len(connection_levels))
    plt.plot(x_positions, median_series, marker="o", color=COLOR[system], label=LABEL[system])
    plt.plot(x_positions, tail_series, marker="o", color=COLOR[system], linestyle="--", alpha=0.6)

plt.yscale("log")                       # values span 0.01 ms to ~60 ms
plt.xticks(range(len(connection_levels)), connection_levels)
plt.xlabel("concurrent connections")
plt.ylabel("latency (ms)")
plt.title("Latency vs concurrency  (solid = median, dashed = p99 tail)")
plt.legend()
plt.grid(True, alpha=0.3)
save("fig1_latency")


# ---------------------------------------------------------------------------
# Figure 2: throughput of the big transfer, one bar per system.
# ---------------------------------------------------------------------------
throughput = {}
for row in rows_for("throughput"):
    throughput[row["system"]] = float(row["mib_s"])

order = ["direct", "natforge", "frp"]
bar_labels = []
bar_heights = []
bar_colors = []
for system in order:
    bar_labels.append(LABEL[system])
    bar_heights.append(throughput[system])
    bar_colors.append(COLOR[system])

plt.figure(figsize=(6, 3.5))
plt.bar(bar_labels, bar_heights, color=bar_colors)
for index, system in enumerate(order):
    height = throughput[system]
    plt.text(index, height, f"{height:.0f}", ha="center", va="bottom")
plt.ylabel("MiB/s")
plt.title("Throughput (10 MiB transfer)")
plt.grid(True, axis="y", alpha=0.3)
save("fig2_throughput")


# ---------------------------------------------------------------------------
# Figure 3: memory used while busy.
# NatForge (Rust, no GC) holds a flat footprint; frp (Go) swings with every GC
# cycle, so it is drawn with a whisker spanning the observed min..max (bar = median).
# ---------------------------------------------------------------------------
memory = {}
for row in rows_for("memory"):
    memory[row["system"]] = row

systems = ["natforge", "frp"]
medians = []
lower_whisker = []
upper_whisker = []
for system in systems:
    row = memory[system]

    median = float(row["rss_mb"])

    # Fall back to the median when a min/max was not recorded.
    minimum_text = row["rss_min"]
    if minimum_text == "":
        minimum_text = row["rss_mb"]
    minimum = float(minimum_text)

    maximum_text = row["rss_max"]
    if maximum_text == "":
        maximum_text = row["rss_mb"]
    maximum = float(maximum_text)

    medians.append(median)
    lower_whisker.append(median - minimum)
    upper_whisker.append(maximum - median)

plt.figure(figsize=(5, 3.8))
plt.bar(
    ["NatForge", "frp"],
    medians,
    color=[COLOR["natforge"], COLOR["frp"]],
    yerr=[lower_whisker, upper_whisker],
    capsize=8,
    ecolor="#333333",
)
for index, system in enumerate(systems):
    row = memory[system]
    median = medians[index]
    top_of_whisker = median + upper_whisker[index]

    # Only show a (min-max) range when it actually varies (frp does, NatForge does not).
    if row["rss_min"] == row["rss_max"]:
        range_text = ""
    else:
        range_text = f"\n({row['rss_min']}-{row['rss_max']})"

    plt.text(index, top_of_whisker, f"{median:.0f} MB{range_text}", ha="center", va="bottom")

plt.ylabel("memory under load (MB)")
plt.title("Memory while handling 100 connections\n(bar = median, whisker = min..max over 12 samples)")
plt.grid(True, axis="y", alpha=0.3)
plt.ylim(0, 420)
save("fig3_memory")

print("wrote fig1_latency, fig2_throughput, fig3_memory")
