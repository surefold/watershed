"""Scores a submission the way the competition does: shapely polygons, no overlaps (touching allowed)."""
import csv, sys
from collections import defaultdict
from shapely import affinity
from shapely.geometry import Polygon
from shapely.strtree import STRtree

TREE = Polygon([(0, .8), (.125, .5), (.0625, .5), (.2, .25), (.1, .25), (.35, 0), (.075, 0), (.075, -.2),
                (-.075, -.2), (-.075, 0), (-.35, 0), (-.1, .25), (-.2, .25), (-.0625, .5), (-.125, .5)])
groups = defaultdict(list)
for row in csv.DictReader(open(sys.argv[1])):
    x, y, deg = (float(row[k][1:]) for k in ("x", "y", "deg"))
    assert -100 <= x <= 100 and -100 <= y <= 100
    groups[int(row["id"].split("_")[0])].append(affinity.translate(affinity.rotate(TREE, deg, origin=(0, 0)), x, y))
assert sorted(groups) == list(range(1, 201)) and all(len(v) == n for n, v in groups.items())
score, overlaps, worst = 0.0, 0, 0.0
for n, polys in groups.items():
    tree = STRtree(polys)
    for i, p in enumerate(polys):
        for j in tree.query(p):
            if j > i and p.intersects(polys[j]) and not p.touches(polys[j]):
                overlaps += 1
                worst = max(worst, p.intersection(polys[j]).area)
    xs = [c for p in polys for c in (p.bounds[0], p.bounds[2])]
    ys = [c for p in polys for c in (p.bounds[1], p.bounds[3])]
    score += max(max(xs) - min(xs), max(ys) - min(ys)) ** 2 / n
print(f"{sys.argv[1]}: score {score:.6f}, overlapping pairs {overlaps} (largest overlap area {worst:.2e})")
