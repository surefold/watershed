"""Writes `household.xlsx`, the workbook the `households` example reads, and
`household_reference.csv`, what LibreOffice makes of it for some households.

The workbook is the kind a welfare rights adviser or a policy researcher
keeps: one household on the `Household` sheet, the rules' numbers on
`Rules`, and the sums on `Sums`: income tax and social insurance on each
adult's pay, a payment for children that high earners pay back, income
support that is withdrawn as the household earns, capped for those who earn
little, and free school meals for those who earn least. It says what the
household has left to live on.

The rules look a little like the United Kingdom's, but every number is made
up and much is left out.

It is written with `openpyxl` and worked out by LibreOffice, which must both
be installed:

    python3 crates/watershed-gpu/examples/shared/household.py

Then LibreOffice is driven directly: the household's cells are set to each
of some made-up households in turn and the workbook recalculated. What it
answers, and how long a household took, go in the reference file for the
example to check itself against.
"""

import csv
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time

import openpyxl
from openpyxl.styles import Font

HERE = os.path.dirname(os.path.abspath(__file__))
REFERENCE_HOUSEHOLDS = 300

# The household's cells, in the order the example and the reference file use.
INPUTS = ["B4", "B5", "B6", "B7", "B8", "B9", "B10"]
# What is read back: what the household has to live on, the tax and social
# insurance it pays, and the support it gets, each for a year.
OUTPUTS = ["B13", "B14", "B15"]

RULES = [
    # (row, what, value)
    (4, "Income tax: pay free of tax", 12570),
    (5, "taken away above this pay, a pound for every two", 100000),
    (6, "Basic rate", 0.20),
    (7, "on this much of taxed pay", 37700),
    (8, "Higher rate", 0.40),
    (9, "up to this much", 125140),
    (10, "Top rate", 0.45),
    (12, "Social insurance: nothing on pay below", 12570),
    (13, "Main rate", 0.08),
    (14, "up to pay of", 50270),
    (15, "Rate above that", 0.02),
    (17, "Child payment a year: the first child", 1355),
    (18, "each other child", 897),
    (19, "paid back from this pay of the higher earner", 60000),
    (20, "a hundredth for every", 200),
    (22, "Income support a month: one adult", 400),
    (23, "a couple", 628),
    (24, "the first child", 339),
    (25, "each other child", 293),
    (26, "children counted, at most", 2),
    (27, "Share of childcare costs met", 0.85),
    (28, "up to, for one child", 1015),
    (29, "up to, for more", 1739),
    (30, "Earnings a month left out, with children", 411),
    (31, "Taken off for each pound earned above that", 0.55),
    (32, "Savings left out", 6000),
    (33, "Taken off a month for every 250 of savings above that", 4.35),
    (34, "No support with savings above", 16000),
    (35, "Support capped a month at, for those earning less than the next line", 1835),
    (36, "No cap from monthly pay of", 793),
    (38, "School meals a child a year", 490),
    (39, "for those on income support taking home no more than", 7400),
]


def rules(ws):
    ws["A1"] = "The rules' numbers"
    ws["A1"].font = Font(bold=True)
    for r, what, value in RULES:
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)
    ws["D3"] = "Rent met a month, at most, by children"
    ws["D3"].font = Font(bold=True)
    ws["D4"], ws["E4"] = "Children from", "Rent"
    for r, (children, rent) in enumerate([(0, 560), (1, 720), (2, 720), (3, 880), (5, 1040)], start=5):
        ws.cell(r, 4, children)
        ws.cell(r, 5, rent)
    ws.column_dimensions["A"].width = 62


def sums(ws):
    ws["A1"] = "The sums"
    ws["A1"].font = Font(bold=True)
    ws["B3"], ws["C3"] = "First adult", "Second adult"
    h, r = "Household!", "Rules!"
    for column, pay in (("B", f"{h}$B$6"), ("C", f"IF({h}$B$4=2,{h}$B$7,0)")):
        c = column
        ws[f"{c}4"] = f"={pay}"
        # The allowance goes, a pound for every two of pay above the limit.
        ws[f"{c}5"] = f"=MAX(0,{r}$B$4-MAX(0,{c}4-{r}$B$5)/2)"
        ws[f"{c}6"] = f"=MAX(0,{c}4-{c}5)"
        ws[f"{c}7"] = (f"={r}$B$6*MIN({c}6,{r}$B$7)+{r}$B$8*MAX(0,MIN({c}6,{r}$B$9)-{r}$B$7)"
                       f"+{r}$B$10*MAX(0,{c}6-{r}$B$9)")
        ws[f"{c}8"] = (f"={r}$B$13*MAX(0,MIN({c}4,{r}$B$14)-{r}$B$12)"
                       f"+{r}$B$15*MAX(0,{c}4-{r}$B$14)")
        ws[f"{c}9"] = f"={c}4-ROUND({c}7,2)-ROUND({c}8,2)"
    for row, what in [(4, "Pay"), (5, "Free of tax"), (6, "Taxed"), (7, "Income tax"),
                      (8, "Social insurance"), (9, "Taken home")]:
        ws.cell(row, 1, what)
    lines = [
        (12, "Child payment", f"=IF({h}B5=0,0,{r}B17+{r}B18*({h}B5-1))"),
        (13, "Paid back by the higher earner",
         f"=B12*MIN(1,ROUNDDOWN(MAX(0,MAX(B4,C4)-{r}B19)/{r}B20,0)/100)"),
        (15, "Income support a month: for the adults", f"=IF({h}B4=2,{r}B23,{r}B22)"),
        (16, "Children counted", f"=MIN({h}B5,{r}B26)"),
        (17, "for the children", f"=IF(B16=0,0,{r}B24+{r}B25*(B16-1))"),
        (18, "for rent", f"=MIN({h}B8,VLOOKUP({h}B5,{r}D5:E9,2,TRUE))"),
        (19, "Every adult works (1) or not (0)", f"=IF(AND(B4>0,OR({h}B4=1,C4>0)),1,0)"),
        (20, "for childcare",
         f"=IF(AND({h}B5>0,B19=1),MIN({r}B27*{h}B9,IF({h}B5=1,{r}B28,{r}B29)),0)"),
        (21, "The most it could be", "=B15+B17+B18+B20"),
        (22, "Taken home a month", "=(B9+C9)/12"),
        (23, "Left out of that", f"=IF({h}B5>0,{r}B30,0)"),
        (24, "Taken off for earnings", f"={r}B31*MAX(0,B22-B23)"),
        (25, "Taken off for savings",
         f"=IF({h}B10>{r}B32,ROUNDUP(({h}B10-{r}B32)/250,0)*{r}B33,0)"),
        (26, "Before the cap", f"=IF({h}B10>{r}B34,0,MAX(0,B21-B24-B25))"),
        # Childcare is paid on top of the cap.
        (27, "Income support a month",
         f"=ROUND(IF((B4+C4)/12>={r}B36,B26,MIN(B26,{r}B35+B20)),2)"),
        (29, "School meals a year",
         f"=IF(AND(B27>0,B9+C9<={r}B39),{r}B38*{h}B5,0)"),
    ]
    for row, what, formula in lines:
        ws.cell(row, 1, what)
        ws.cell(row, 2, formula)
    ws.column_dimensions["A"].width = 40


def household(ws):
    ws["A1"] = "What a household has to live on"
    ws["A3"] = "The household"
    for r, (what, value) in enumerate([
        ("Adults (1 or 2)", 2),
        ("Children", 2),
        ("First adult's pay a year", 24000),
        ("Second adult's pay a year", 6000),
        ("Rent a month", 750),
        ("Childcare a month", 300),
        ("Savings", 1500),
    ], start=4):
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)
    ws["A12"] = "For a year"
    ws["A13"] = "To live on, before rent and childcare"
    ws["B13"] = "=Sums!B9+Sums!C9+B15"
    ws["A14"] = "Income tax and social insurance"
    ws["B14"] = "=ROUND(Sums!B7,2)+ROUND(Sums!B8,2)+ROUND(Sums!C7,2)+ROUND(Sums!C8,2)"
    ws["A15"] = "Support: child payment, income support, school meals"
    ws["B15"] = "=Sums!B12-Sums!B13+12*Sums!B27+Sums!B29"
    for cell in ("A1", "A3", "A12"):
        ws[cell].font = Font(bold=True)
    ws.column_dimensions["A"].width = 54


def write(path):
    wb = openpyxl.Workbook()
    sheet = wb.active
    sheet.title = "Household"
    household(sheet)
    sums(wb.create_sheet("Sums"))
    rules(wb.create_sheet("Rules"))
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


def made_up_household(rng):
    """One household of a made-up country, of the kind `households.rs` makes."""
    adults = 2.0 if rng.unit() < 0.6 else 1.0
    children = float([0, 0, 0, 0, 1, 1, 2, 2, 3, 4][int(10 * rng.unit())])
    pay = lambda out: 0.0 if rng.unit() < out else round(9000 * math.exp(2.4 * rng.unit() ** 1.3), -2)
    first = pay(0.15)
    second = pay(0.35) if adults == 2 else 0.0
    rent = 0.0 if rng.unit() < 0.45 else round(400 + 1100 * rng.unit(), -1)
    childcare = round(1200 * rng.unit(), -1) if children > 0 and rng.unit() < 0.5 else 0.0
    savings = round(60000 * rng.unit() ** 6, -2)
    return [adults, children, first, second, rent, childcare, savings]


def reference(workbook, out):
    """Drives LibreOffice through some households, one at a time."""
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
        sheet = doc.Sheets.getByName("Household")
        rng = Rng(0x9E3779B97F4A7C15)
        households = [made_up_household(rng) for _ in range(REFERENCE_HOUSEHOLDS)]
        rows = []
        started = time.perf_counter()
        for one in households:
            for cell, value in zip(INPUTS, one):
                sheet.getCellRangeByName(cell).Value = value
            doc.calculateAll()
            rows.append(one + [sheet.getCellRangeByName(c).Value for c in OUTPUTS])
        each = (time.perf_counter() - started) / len(households)
        doc.close(True)
        with open(out, "w", newline="") as f:
            f.write(f"# LibreOffice recalculating household.xlsx for each household: "
                    f"{each * 1000:.2f} ms a household\n")
            w = csv.writer(f)
            w.writerow(["adults", "children", "first_pay", "second_pay", "rent", "childcare",
                        "savings", "to_live_on", "tax", "support"])
            for row in rows:
                w.writerow([repr(float(x)) for x in row])
        print(f"LibreOffice: {each * 1000:.2f} ms a household", file=sys.stderr)
    finally:
        office.terminate()


def main():
    target = os.path.join(HERE, "household.xlsx")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "household.xlsx")
        write(path)
        worked_out(path, HERE)
    print("wrote", target, file=sys.stderr)
    if "--no-reference" not in sys.argv:
        reference(target, os.path.join(HERE, "household_reference.csv"))


if __name__ == "__main__":
    main()
