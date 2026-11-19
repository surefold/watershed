"""Writes `illustration.xlsx`, the workbook the `illustrations` example reads,
and `illustration_reference.csv`, what LibreOffice makes of it for some
members.

The workbook is the kind a pension scheme's administrator keeps for the
yearly statement: one member on the `Member` sheet, what is assumed on
`Assumptions`, and the member's pot followed year by year to retirement on
`Projection`, at a low, a middle and a high rate of growth. It says what the
pot might buy, in today's money, and whether that is the income the member
should aim for. Every number in it is made up; the shape is the usual one.

It is written with `openpyxl` and worked out by LibreOffice, which must both
be installed:

    python3 crates/watershed-gpu/examples/shared/illustration.py

Then LibreOffice is driven directly: the member's cells are set to each of
some made-up members in turn and the workbook recalculated, as a mail merge
of statements would. What it answers, and how long a member took, go in the
reference file for the example to check itself against.
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
YEARS = 58  # From today to the last year anyone could still be working.
FIRST = 5
LAST = FIRST + YEARS - 1
REFERENCE_MEMBERS = 300

# The member's cells, in the order the example and the reference file use.
INPUTS = ["B4", "B5", "B6", "B7", "B8", "B9"]
# What is read back: the pot at retirement and the income a year on the
# middle figures, the income on the low and the high, whether the member is
# on track, and how far short a year.
OUTPUTS = ["C13", "C17", "B17", "D17", "B21", "B22"]


def assumptions(ws):
    ws["A1"] = "What is assumed"
    for r, (what, value) in enumerate([
        ("Prices rise each year by", 0.025),
        ("Pay rises faster than prices by", 0.01),
        ("Charged on the pot each year", 0.005),
        ("Growth a year: low", 0.02),
        ("Growth a year: middle", 0.05),
        ("Growth a year: high", 0.08),
        ("Pay below this earns no contribution", 6240),
        ("nor pay above this", 50270),
        ("Share of the pot taken as cash, tax free", 0.25),
        ("State pension a year", 11500),
        ("paid from age", 67),
        ("Growth of what is moved out of shares near retirement", 0.03),
    ], start=4):
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)

    def table(col, heads, rows):
        for c, head in enumerate(heads):
            ws.cell(4, col + c, head).font = Font(bold=True)
        for r, row in enumerate(rows, start=5):
            for c, value in enumerate(row):
                ws.cell(r, col + c, value)

    table(4, ["Retiring from age", "Income a year for each 1 of pot"], [
        (55, 0.050), (58, 0.053), (60, 0.056), (62, 0.059), (64, 0.062),
        (65, 0.064), (66, 0.066), (67, 0.068), (68, 0.070), (70, 0.075)])
    table(7, ["Years to retirement from", "Share still in shares"], [
        (0, 0.4), (3, 0.55), (5, 0.7), (7, 0.85), (10, 1.0)])
    table(10, ["Pay from", "Share of pay to aim for in retirement"], [
        (0, 0.80), (15000, 0.70), (27000, 0.67), (37000, 0.60), (62000, 0.50)])
    ws.column_dimensions["A"].width = 50


# Where each rate of growth's columns start on Projection, and its cell.
RATES = [("Low", 11, "$B$7"), ("Middle", 15, "$B$8"), ("High", 19, "$B$9")]


def col(n):
    return openpyxl.utils.get_column_letter(n)


def projection(ws):
    ws["A1"] = "The pot, year by year"
    heads = ["Years from now", "Age", "At work", "Pay", "Pay that counts", "Paid in",
             "Years to retirement", "Share in shares", "Worth in today's money", "Retires this year"]
    for c, head in enumerate(heads, start=1):
        ws.cell(4, c, head).font = Font(bold=True)
    for name, start, _ in RATES:
        ws.cell(3, start, name).font = Font(bold=True)
        for k, head in enumerate(["Pot at the start", "Growth", "Pot at the end",
                                  "At retirement, in today's money"]):
            ws.cell(4, start + k, head).font = Font(bold=True)
    a = "Assumptions!"
    for t in range(YEARS):
        r = FIRST + t
        ws.cell(r, 1, t)
        ws.cell(r, 2, f"=Member!$B$4+A{r}")
        ws.cell(r, 3, f"=IF(B{r}<Member!$B$9,1,0)")
        ws.cell(r, 4, f"=Member!$B$5*(1+{a}$B$4+{a}$B$5)^A{r}")
        # The band of pay that earns a contribution moves with prices.
        ws.cell(r, 5, f"=MAX(0,MIN(D{r},{a}$B$11*(1+{a}$B$4)^A{r})-{a}$B$10*(1+{a}$B$4)^A{r})")
        ws.cell(r, 6, f"=C{r}*E{r}*(Member!$B$7+Member!$B$8)")
        ws.cell(r, 7, f"=MAX(0,Member!$B$9-B{r})")
        ws.cell(r, 8, f"=VLOOKUP(G{r},{a}$G$5:$H$9,2,TRUE)")
        ws.cell(r, 9, f"=1/(1+{a}$B$4)^A{r}")
        ws.cell(r, 10, f"=IF(B{r}=Member!$B$9,1,0)")
        for _, start, rate in RATES:
            pot, growth, end, at_retirement = (col(start + k) for k in range(4))
            ws.cell(r, start, "=Member!$B$6" if t == 0 else f"={end}{r - 1}")
            ws.cell(r, start + 1, f"={a}$B$15+({a}{rate}-{a}$B$15)*$H{r}")
            # What is paid in arrives through the year, so grows for half of it.
            ws.cell(r, start + 2,
                    f"=({pot}{r}*(1+{growth}{r})+$F{r}*(1+{growth}{r})^0.5)*(1-{a}$B$6)")
            ws.cell(r, start + 3, f"=$J{r}*{pot}{r}*$I{r}")


def member(ws):
    ws["A1"] = "What your pension might be"
    ws["A3"] = "The member"
    for r, (what, value) in enumerate([
        ("Age", 42),
        ("Pay a year", 34000),
        ("Pot today", 41000),
        ("Paid in by the member, as a share of pay that counts", 0.05),
        ("Paid in by the employer", 0.03),
        ("Retires at", 67),
    ], start=4):
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)
    ws["A11"] = "Income a year for each 1 of pot, at that age"
    ws["B11"] = "=VLOOKUP(B9,Assumptions!D5:E14,2,TRUE)"
    for c, head in enumerate(["Low", "Middle", "High"], start=2):
        ws.cell(12, c, head).font = Font(bold=True)
    for c, (_, start, _) in enumerate(RATES, start=2):
        me = col(c)
        at_retirement = col(start + 3)
        ws.cell(13, c, f"=SUM(Projection!{at_retirement}{FIRST}:{at_retirement}{LAST})")
        ws.cell(14, c, f"=ROUND({me}13*Assumptions!$B$12,-2)")
        ws.cell(15, c, f"=ROUND(({me}13-{me}14)*$B$11,-1)")
        ws.cell(16, c, "=Assumptions!$B$13")
        ws.cell(17, c, f"={me}15+{me}16")
        ws.cell(18, c, f"={me}17/$B$5")
    for r, what in enumerate([
        "Pot at retirement, in today's money", "Taken as cash, tax free",
        "Income a year from the rest", "State pension, once old enough for it",
        "Income a year in retirement", "As a share of pay today"], start=13):
        ws.cell(r, 1, what)
    ws["A20"], ws["B20"] = "Share of pay to aim for", "=VLOOKUP(B5,Assumptions!J5:K9,2,TRUE)"
    ws["A21"], ws["B21"] = "On track (1) or not (0), on the middle figures", "=IF(C18>=B20,1,0)"
    ws["A22"], ws["B22"] = "Short of it, a year", "=MAX(0,ROUND(B20*B5-C17,-1))"
    for cell in ("A1", "A3"):
        ws[cell].font = Font(bold=True)
    ws.column_dimensions["A"].width = 50


def write(path):
    wb = openpyxl.Workbook()
    sheet = wb.active
    sheet.title = "Member"
    member(sheet)
    projection(wb.create_sheet("Projection"))
    assumptions(wb.create_sheet("Assumptions"))
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


def made_up_member(rng):
    """One member of a made-up scheme, of the kind `illustrations.rs` makes."""
    age = float(22 + int(43 * rng.unit()))
    pay = round(17000 * math.exp(1.9 * rng.unit() ** 1.5), -2)
    # A pot is some years of contributions, more for the older.
    pot = round(pay * 0.08 * (age - 21) * (0.2 + 1.6 * rng.unit()), -2)
    pays = [0.03, 0.04, 0.05, 0.05, 0.05, 0.06, 0.08, 0.10][int(8 * rng.unit())]
    employer = [0.03, 0.03, 0.04, 0.05, 0.06, 0.08][int(6 * rng.unit())]
    retires = max(age + 1, [60.0, 63.0, 65.0, 66.0, 67.0, 67.0, 67.0, 68.0][int(8 * rng.unit())])
    return [age, pay, pot, pays, employer, retires]


def reference(workbook, out):
    """Drives LibreOffice through some members, one at a time."""
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
        sheet = doc.Sheets.getByName("Member")
        rng = Rng(0x9E3779B97F4A7C15)
        members = [made_up_member(rng) for _ in range(REFERENCE_MEMBERS)]
        rows = []
        started = time.perf_counter()
        for one in members:
            for cell, value in zip(INPUTS, one):
                sheet.getCellRangeByName(cell).Value = value
            doc.calculateAll()
            rows.append(one + [sheet.getCellRangeByName(c).Value for c in OUTPUTS])
        each = (time.perf_counter() - started) / len(members)
        doc.close(True)
        with open(out, "w", newline="") as f:
            f.write(f"# LibreOffice recalculating illustration.xlsx for each member: "
                    f"{each * 1000:.1f} ms a member\n")
            w = csv.writer(f)
            w.writerow(["age", "pay", "pot", "pays", "employer", "retires",
                        "pot_at_retirement", "income", "income_low", "income_high",
                        "on_track", "short"])
            for row in rows:
                w.writerow([repr(float(x)) for x in row])
        print(f"LibreOffice: {each * 1000:.1f} ms a member", file=sys.stderr)
    finally:
        office.terminate()


def main():
    target = os.path.join(HERE, "illustration.xlsx")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "illustration.xlsx")
        write(path)
        worked_out(path, HERE)
    print("wrote", target, file=sys.stderr)
    if "--no-reference" not in sys.argv:
        reference(target, os.path.join(HERE, "illustration_reference.csv"))


if __name__ == "__main__":
    main()
