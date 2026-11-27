"""Analyse un CSV PresentMon v1 limité à la séquence de gameplay."""
import argparse
import csv
import json
import math
import statistics
from pathlib import Path


def summarize(stream, application="victory.exe"):
    reader = csv.DictReader(stream)
    required = {"Application", "ProcessID", "SwapChainAddress", "MsBetweenPresents"}
    if not required.issubset(reader.fieldnames or []):
        raise ValueError("Exporter le CSV PresentMon au format v1.")
    samples = []
    chains = set()
    for row in reader:
        if row["Application"].casefold() != application.casefold():
            continue
        chains.add((row["ProcessID"], row["SwapChainAddress"]))
        value = float(row["MsBetweenPresents"])
        if not samples and value == 0:
            continue
        if not math.isfinite(value) or value <= 0:
            raise ValueError(f"Intervalle invalide à la ligne {reader.line_num}")
        samples.append(value)
    if len(chains) != 1 or len(samples) < 2:
        raise ValueError("Il faut une capture d'un processus, une swapchain et deux intervalles au minimum.")
    samples.sort()
    return dict(intervals=len(samples), duration_s=sum(samples)/1000,
                fps=1000/statistics.mean(samples), median_ms=statistics.median(samples),
                p95_ms=samples[math.ceil(0.95*len(samples))-1])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("csv", type=Path)
    args = parser.parse_args()
    with args.csv.open(encoding="utf-8-sig", newline="") as stream:
        print(json.dumps(summarize(stream), indent=2))
