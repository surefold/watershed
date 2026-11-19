"""Writes `project.xlsx`, the workbook the `project_risk` example reads, and
`project_reference.csv`, what LibreOffice makes of it for some trials.

The workbook is the kind an estimator keeps for a building project, here an
upgrade to a water treatment works: the cost estimate line by line on
`Estimate`, the programme of activities and what each waits for on
`Programme`, the risk register on `Risks`, and the totals on `Summary`.
Each rate and each duration has a low, a likely and a high figure beside
the one used; each risk has a chance, and a low, likely and high cost and
delay. The workbook as saved uses the likely figures and has no risk
happen. Every name and number in it is made up.

It is written with `openpyxl` and worked out by LibreOffice, which must both
be installed:

    python3 crates/watershed-gpu/examples/shared/project.py

Then LibreOffice is driven directly: the figures used are set to a draw
between each low and high, the risks to happen or not by their chances, and
the workbook recalculated, as a risk add-in does for each of its trials.
What it answers, and how long a trial took, go in the reference file for
the example to check itself against.
"""

import csv
import os
import shutil
import subprocess
import sys
import tempfile
import time

import openpyxl
from openpyxl.styles import Font

HERE = os.path.dirname(os.path.abspath(__file__))
FIRST = 5
REFERENCE_TRIALS = 300

# (activity, low, likely, high in weeks, the activities it waits for)
ACTIVITIES = [
    ("Design", 16, 20, 30, []),
    ("Planning consent", 20, 26, 52, []),
    ("Order and make the plant", 24, 30, 44, [1]),
    ("Set up the site", 3, 4, 6, [1, 2]),
    ("Enabling works", 6, 8, 12, [4]),
    ("Piling", 4, 6, 10, [5]),
    ("Foundations and floors", 11, 14, 20, [6]),
    ("Tanks", 16, 20, 28, [7]),
    ("Buildings", 14, 18, 26, [7]),
    ("Pipework", 9, 12, 18, [8]),
    ("Install the plant", 12, 16, 24, [3, 9]),
    ("Power and controls", 8, 10, 15, [11]),
    ("Commissioning", 6, 8, 14, [10, 12]),
    ("Landscaping", 4, 6, 9, [10]),
    ("Handover", 2, 2, 4, [13, 14]),
]

# (line, quantity, unit, low, likely, high rate). A quantity of None is a
# cost for each week the project runs.
LINES = [
    ("Design fees", 1, "sum", 820000, 900000, 1150000),
    ("Surveys and ground investigation", 1, "sum", 110000, 140000, 220000),
    ("Site set-up and removal", 1, "sum", 260000, 300000, 380000),
    ("Demolition", 1800, "m3", 38, 46, 70),
    ("Excavation", 14500, "m3", 17, 21, 34),
    ("Disposal of spoil", 9800, "m3", 26, 32, 55),
    ("Piles", 420, "each", 2300, 2700, 3900),
    ("Concrete", 6900, "m3", 215, 245, 320),
    ("Reinforcement", 880, "t", 1250, 1450, 2100),
    ("Formwork", 12400, "m2", 58, 68, 92),
    ("Tank linings", 5200, "m2", 74, 88, 125),
    ("Steel frame", 310, "t", 2700, 3100, 4300),
    ("Cladding and roofing", 4800, "m2", 125, 145, 195),
    ("Pipework", 3400, "m", 310, 360, 520),
    ("Valves and fittings", 640, "each", 1150, 1350, 1900),
    ("Pumps", 18, "each", 41000, 47000, 66000),
    ("Screens and filters", 6, "each", 235000, 270000, 370000),
    ("Blowers and aeration", 1, "sum", 760000, 860000, 1180000),
    ("Power supply", 1, "sum", 540000, 620000, 900000),
    ("Controls and software", 1, "sum", 470000, 560000, 860000),
    ("Roads and paving", 6200, "m2", 62, 72, 98),
    ("Landscaping", 1, "sum", 120000, 150000, 210000),
    ("Commissioning and training", 1, "sum", 310000, 360000, 520000),
    ("Site managers and engineers", None, "week", 21000, 24000, 30000),
    ("Site offices, welfare and security", None, "week", 6500, 7500, 9500),
    ("Cranes and plant on hire", None, "week", 13000, 15500, 21000),
    ("Insurance and bonds", None, "week", 3800, 4200, 5200),
]

# (risk, chance, low, likely, high cost, low, likely, high delay in weeks,
# the activity delayed)
RISKS = [
    ("Ground worse than the investigation found", 0.30, 150000, 320000, 700000, 2, 4, 9, 6),
    ("Planning consent challenged", 0.20, 20000, 60000, 150000, 8, 14, 30, 2),
    ("Plant delivered late", 0.25, 0, 50000, 200000, 4, 8, 18, 3),
    ("Asbestos in the old works", 0.15, 80000, 180000, 420000, 2, 4, 8, 5),
    ("The client changes the design", 0.35, 120000, 300000, 800000, 2, 4, 8, 1),
    ("Services not where the drawings say", 0.25, 40000, 110000, 300000, 1, 3, 6, 5),
    ("Excavation floods", 0.10, 60000, 150000, 400000, 2, 3, 7, 7),
    ("A subcontractor fails", 0.05, 400000, 900000, 2200000, 6, 12, 24, 8),
    ("Bad winter", 0.40, 20000, 60000, 160000, 1, 3, 6, 8),
    ("Archaeology", 0.05, 50000, 200000, 600000, 4, 10, 22, 5),
    ("Commissioning fails its tests", 0.15, 100000, 250000, 600000, 2, 5, 12, 13),
    ("Power connection late", 0.20, 0, 30000, 120000, 3, 6, 14, 12),
    ("Tank fails its water test", 0.10, 90000, 220000, 500000, 2, 4, 8, 10),
    ("Theft or damage on site", 0.20, 15000, 40000, 110000, 0, 1, 2, 9),
]


def write(path):
    wb = openpyxl.Workbook()

    estimate = wb.active
    estimate.title = "Estimate"
    estimate["A1"] = "The estimate"
    for c, head in enumerate(["Item", "Quantity", "Unit", "Rate: low", "likely", "high",
                              "Rate used", "Cost"], start=1):
        estimate.cell(4, c, head).font = Font(bold=True)
    for r, (item, quantity, unit, low, likely, high) in enumerate(LINES, start=FIRST):
        estimate.cell(r, 1, item)
        estimate.cell(r, 3, unit)
        for c, value in enumerate([low, likely, high, likely], start=4):
            estimate.cell(r, c, value)
        if quantity is None:
            # For each week the project runs; prices move with the market
            # all the same.
            estimate.cell(r, 2, "=Summary!$B$6")
        else:
            estimate.cell(r, 2, quantity)
        estimate.cell(r, 8, f"=B{r}*G{r}*Summary!$B$4")
    last_line = FIRST + len(LINES) - 1

    programme = wb.create_sheet("Programme")
    programme["A1"] = "The programme, in weeks"
    for c, head in enumerate(["Activity", "Weeks: low", "likely", "high", "Weeks used",
                              "Delayed by risks", "Starts", "Finishes"], start=1):
        programme.cell(4, c, head).font = Font(bold=True)
    for r, (activity, low, likely, high, waits) in enumerate(ACTIVITIES, start=FIRST):
        programme.cell(r, 1, activity)
        for c, value in enumerate([low, likely, high, likely], start=2):
            programme.cell(r, c, value)
        delays = [f"Risks!M{FIRST + k}" for k, risk in enumerate(RISKS) if risk[8] == r - FIRST + 1]
        programme.cell(r, 6, "=" + "+".join(delays) if delays else 0)
        finishes = [f"H{FIRST + w - 1}" for w in waits]
        programme.cell(r, 7, f"=MAX({','.join(finishes)})" if finishes else 0)
        programme.cell(r, 8, f"=G{r}+E{r}+F{r}")
    last_activity = FIRST + len(ACTIVITIES) - 1

    risks = wb.create_sheet("Risks")
    risks["A1"] = "The risk register"
    for c, head in enumerate(["Risk", "Chance", "Cost: low", "likely", "high", "Weeks: low",
                              "likely", "high", "Happens (1) or not (0)", "Cost used",
                              "Weeks used", "Cost", "Delay", "Delays activity"], start=1):
        risks.cell(4, c, head).font = Font(bold=True)
    for r, (risk, chance, c1, c2, c3, w1, w2, w3, activity) in enumerate(RISKS, start=FIRST):
        risks.cell(r, 1, risk)
        for c, value in enumerate([chance, c1, c2, c3, w1, w2, w3, 0, c2, w2], start=2):
            risks.cell(r, c, value)
        risks.cell(r, 12, f"=I{r}*J{r}")
        risks.cell(r, 13, f"=I{r}*K{r}")
        risks.cell(r, 14, ACTIVITIES[activity - 1][0])
    last_risk = FIRST + len(RISKS) - 1

    summary = wb.create_sheet("Summary")
    summary["A1"] = "The project"
    for r, (what, value) in enumerate([
        ("Prices against today's (the market)", 1.0),
        ("", None),
        ("Weeks to finish", f"=MAX(Programme!H{FIRST}:H{last_activity})"),
        ("The estimate", f"=SUM(Estimate!H{FIRST}:H{last_line})"),
        ("Risks that happen", f"=SUM(Risks!L{FIRST}:L{last_risk})"),
        ("Weeks promised in the contract", 140),
        ("Damages for each week late", 30000),
        ("Damages at most", 1500000),
        ("Damages", "=MIN(B11,B10*MAX(0,B6-B9))"),
        ("Cost", "=ROUND(B7+B8+B12,0)"),
        ("Budget", 29000000),
        ("Over budget (1) or not (0)", "=IF(B13>B14,1,0)"),
        ("Late (1) or not (0)", "=IF(B6>B9,1,0)"),
    ], start=4):
        summary.cell(r, 1, what)
        if value is not None:
            summary.cell(r, 2, value)
    summary["C3"], summary["D3"] = "low", "high"
    summary["C4"], summary["D4"] = 0.94, 1.22
    for ws, width in ((summary, 40), (risks, 40), (programme, 28), (estimate, 36)):
        ws.column_dimensions["A"].width = width
    wb.save(path)


def worked_out(path, out_dir):
    """Has LibreOffice work out every cell and save the workbook again."""
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(
            ["soffice", "--headless", "--calc", "--convert-to", "xlsx", "--outdir", tmp, path],
            check=True,
            capture_output=True,
        )
        shutil.move(os.path.join(tmp, os.path.basename(path)), os.path.join(out_dir, os.path.basename(path)))


class Rng:
    """A small deterministic random number generator, the example's own kind."""

    def __init__(self, seed):
        self.state = seed

    def unit(self):
        s = self.state
        s ^= (s << 13) & 0xFFFFFFFFFFFFFFFF
        s ^= s >> 7
        s ^= (s << 17) & 0xFFFFFFFFFFFFFFFF
        self.state = s
        return ((s >> 11) + 0.5) / float(1 << 53)

    def between(self, low, likely, high):
        """A draw from the triangle on low, likely and high."""
        u, span = self.unit(), high - low
        if span <= 0:
            return float(likely)
        if u * span < likely - low:
            return low + (u * span * (likely - low)) ** 0.5
        return high - ((1 - u) * span * (high - likely)) ** 0.5


def cells():
    """The cells a trial sets, in the order the example and the reference use."""
    out = ["Summary!B4"]
    out += [f"Estimate!G{FIRST + k}" for k in range(len(LINES))]
    out += [f"Programme!E{FIRST + k}" for k in range(len(ACTIVITIES))]
    for k in range(len(RISKS)):
        out += [f"Risks!I{FIRST + k}", f"Risks!J{FIRST + k}", f"Risks!K{FIRST + k}"]
    return out


def trial(rng):
    """One trial's figures, in the order of `cells`."""
    out = [rng.between(0.94, 1.0, 1.22)]
    out += [rng.between(low, likely, high) for _, _, _, low, likely, high in LINES]
    out += [rng.between(low, likely, high) for _, low, likely, high, _ in ACTIVITIES]
    for _, chance, c1, c2, c3, w1, w2, w3, _ in RISKS:
        out += [1.0 if rng.unit() < chance else 0.0, rng.between(c1, c2, c3), rng.between(w1, w2, w3)]
    return out


def reference(workbook, out):
    """Drives LibreOffice through some trials, one at a time."""
    import uno  # noqa: F401  (LibreOffice's own Python bridge)
    from com.sun.star.beans import PropertyValue
    from com.sun.star.connection import NoConnectException

    port = 2002
    office = subprocess.Popen(
        ["soffice", "--headless", "--norestore", f"--accept=socket,host=localhost,port={port};urp;"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        local = uno.getComponentContext()
        resolver = local.ServiceManager.createInstanceWithContext(
            "com.sun.star.bridge.UnoUrlResolver", local)
        for _ in range(120):
            try:
                context = resolver.resolve(
                    f"uno:socket,host=localhost,port={port};urp;StarOffice.ComponentContext")
                break
            except NoConnectException:
                time.sleep(0.5)
        else:
            raise SystemExit("LibreOffice did not start")
        desktop = context.ServiceManager.createInstanceWithContext(
            "com.sun.star.frame.Desktop", context)
        hidden = PropertyValue()
        hidden.Name, hidden.Value = "Hidden", True
        doc = desktop.loadComponentFromURL(
            uno.systemPathToFileUrl(workbook), "_blank", 0, (hidden,))
        where = [name.split("!") for name in cells()]
        sheets = {name: doc.Sheets.getByName(name) for name in {sheet for sheet, _ in where}}
        summary = sheets["Summary"]
        rng = Rng(0x9E3779B97F4A7C15)
        trials = [trial(rng) for _ in range(REFERENCE_TRIALS)]
        rows = []
        started = time.perf_counter()
        for one in trials:
            for (sheet, cell), value in zip(where, one):
                sheets[sheet].getCellRangeByName(cell).Value = value
            doc.calculateAll()
            rows.append(one + [summary.getCellRangeByName(c).Value for c in ("B13", "B6")])
        each = (time.perf_counter() - started) / len(trials)
        doc.close(True)
        with open(out, "w", newline="") as f:
            f.write(f"# LibreOffice recalculating project.xlsx for each trial: "
                    f"{each * 1000:.1f} ms a trial\n")
            w = csv.writer(f)
            w.writerow(cells() + ["cost", "weeks"])
            for row in rows:
                w.writerow([repr(float(x)) for x in row])
        print(f"LibreOffice: {each * 1000:.1f} ms a trial", file=sys.stderr)
    finally:
        office.terminate()


def main():
    target = os.path.join(HERE, "project.xlsx")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "project.xlsx")
        write(path)
        worked_out(path, HERE)
    print("wrote", target, file=sys.stderr)
    if "--no-reference" not in sys.argv:
        reference(target, os.path.join(HERE, "project_reference.csv"))


if __name__ == "__main__":
    main()
