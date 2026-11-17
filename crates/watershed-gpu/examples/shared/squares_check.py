"""Scores packings from the `squares` example with the competition's own metric.

The metric is the `metric.py` of Kaggle's "Squares in Squares" competition
(its Data tab has it). It is not in this repository. It certifies with
interval arithmetic that no two squares overlap, and measures the side of
the square the packing fills.

    pip install mpmath pandas
    python squares_check.py path/to/metric.py packing.csv [more.csv ...]

Each file is one packing, as the example writes it. A packing the metric
refuses is given the grid's side, which is what the competition does.
"""
import importlib.util
import math
import sys

import pandas as pd

spec = importlib.util.spec_from_file_location("metric", sys.argv[1])
metric = importlib.util.module_from_spec(spec)
spec.loader.exec_module(metric)

for path in sys.argv[2:]:
    rows = pd.read_csv(path, dtype=str).to_dict("records")
    n = int(rows[0]["n"])
    _, score, side, reason = metric.evaluate_single_puzzle(n, rows)
    grid = math.ceil(math.sqrt(n))
    if side is None:
        print(f"{path}: {n} squares REFUSED ({reason}); scored as the grid, {score:.6f}")
    else:
        print(f"{path}: {n} squares, {reason}: side {side[:14]}, score {score:.6f} (the grid: {grid}, {grid / math.sqrt(n) - 1:.6f})")
