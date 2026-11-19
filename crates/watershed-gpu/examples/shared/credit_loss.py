"""Writes `credit_loss.xlsx`, the workbook the `credit_loss` example reads, and
`credit_loss_reference.csv`, what LibreOffice makes of it for some loans.

The workbook is the kind a credit risk team keeps: one mortgage on the
`Loan` sheet, three paths the economy might take on `Economy`, the loan
worked through each of them quarter by quarter on `Schedule`, and the bank's
choices on `Model`. A property is sold some quarters after a default, at
that quarter's prices on each path; a loan already in default is given the
loss on such a sale. Everything in it is made up; the shape is the usual one
for IFRS 9.

It is written with `openpyxl` and worked out by LibreOffice, which must both
be installed:

    python3 crates/watershed-gpu/examples/shared/credit_loss.py

It also writes `credit_loss_changed.xlsx`, a later version of the workbook
for the `audit` example to compare with the first. Two things differ in it.
One is meant: a loan's chance of default must now be three times what was
expected, not twice, for it to move to stage 2 (`Model!B18`). The other is a slip of the kind a copied formula
makes: in quarter 57 of the better path, the chance of default reads the
expected path's unemployment (`Schedule!R61`).

Then LibreOffice is driven directly: the loan's cells are set to each of
some made-up loans in turn and the workbook recalculated, as a macro looping
over a loan book would. What it answers, and how long a loan took, go in the
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
QUARTERS = 100
FIRST = 5  # The first quarter's row on Economy and Schedule.
LAST = FIRST + QUARTERS - 1
# The economy runs on past the last quarter, for a property repossessed then
# and sold some quarters later.
AFTER = 12
REFERENCE_LOANS = 300

# The loan's cells, in the order the example and the reference file use.
INPUTS = ["B4", "B5", "B6", "B7", "B8", "B9", "B10"]
# What is read back: stage, provision, a year's loss, a lifetime's loss.
OUTPUTS = ["B13", "B14", "B15", "B16"]


def economy(ws):
    """Three paths: what the economists expect, a better one and a slump."""
    ws["A1"] = "The economy, quarter by quarter"
    ws["A2"] = ("Unemployment in points; house prices against today's (1 = today). The slump is "
                "the expected path plus the slump's own columns, times how bad it is (Model!B8).")
    heads = ["Quarter", "Expected: unemployment", "Expected: house prices",
             "Better: unemployment", "Better: house prices",
             "Slump adds: unemployment", "Slump adds: house prices",
             "Slump: unemployment", "Slump: house prices"]
    for c, head in enumerate(heads, start=1):
        ws.cell(4, c, head).font = Font(bold=True)
    for t in range(1, QUARTERS + AFTER + 1):
        r = FIRST + t - 1
        # Expected: a small rise in unemployment that passes; prices up 2% a year.
        base_u = 4.5 + 0.4 * math.exp(-((t - 4) / 5.0) ** 2)
        base_h = 1.005 ** t
        # Better: unemployment eases for three years; prices up faster at first.
        good_u = 4.5 - 0.7 * (1 - math.exp(-t / 3.0)) * math.exp(-t / 24.0)
        good_h = base_h * (1 + 0.06 * (1 - math.exp(-t / 6.0)))
        # The slump: unemployment up four points within two years, prices down a fifth.
        rise = 4.0 * (t / 6.0) * math.exp(1 - t / 6.0)
        fall = -0.22 * (1 - math.exp(-t / 3.0)) * (0.35 + 0.65 * math.exp(-t / 20.0))
        ws.cell(r, 1, t)
        ws.cell(r, 2, round(base_u, 3))
        ws.cell(r, 3, round(base_h, 4))
        ws.cell(r, 4, round(good_u, 3))
        ws.cell(r, 5, round(good_h, 4))
        ws.cell(r, 6, round(rise, 3))
        ws.cell(r, 7, round(fall, 4))
        ws.cell(r, 8, f"=B{r}+Model!$B$8*F{r}")
        ws.cell(r, 9, f"=MAX(0.3,C{r}+Model!$B$8*G{r})")


def model(ws, changed):
    """The bank's choices."""
    ws["A1"] = "The model's numbers"
    rows = [
        (3, "How likely each path is", None),
        (4, "Expected", "=1-B5-B6"),
        (5, "Better", 0.2),
        (6, "Slump", 0.3),
        (8, "How bad the slump is (1 = as given)", 1),
        (10, "Unemployment in an ordinary year", 4.5),
        (11, "Defaults rise with each point of unemployment by", 0.25),
        (12, "and with the loan above this share of the property's value", 0.8),
        (13, "by, for each further whole of it", 2.0),
        (14, "Quarters from a default to the property's sale", 6),
        (15, "Cost of selling a repossessed property", 0.07),
        (16, "Least share of a defaulted loan that is lost", 0.05),
        (18, "Looking shakier: the chance of default over its life is this many times what was expected",
         3.0 if changed else 2.0),
        (19, "and higher by at least", 0.01),
        (20, "or it is more than this many days past due", 30),
        (21, "In default: days past due", 90),
    ]
    for r, what, value in rows:
        ws.cell(r, 1, what)
        if value is not None:
            ws.cell(r, 2, value)
    ws["D3"] = "A forced sale fetches less: by how much, by loan to value"
    ws["D4"], ws["E4"] = "Loan to value from", "Discount"
    for r, (ltv, cut) in enumerate([(0, 0.10), (0.6, 0.14), (0.8, 0.18), (0.9, 0.22),
                                    (1.0, 0.26), (1.2, 0.30)], start=5):
        ws.cell(r, 4, ltv)
        ws.cell(r, 5, cut)


# Where each path's columns start on Schedule, and its columns on Economy.
PATHS = [("Expected", 8, "B", "C"), ("Better", 16, "D", "E"), ("Slump", 24, "H", "I")]


def col(n):
    return openpyxl.utils.get_column_letter(n)


def schedule(ws, changed):
    """The loan, quarter by quarter, on each path."""
    ws["A1"] = "The loan, quarter by quarter"
    ws["A2"], ws["B2"] = "Payment each quarter", "=ROUND(PMT(Loan!B5/4,Loan!B6,-Loan!B4),2)"
    ws["A3"], ws["B3"] = "Chance of default in a quarter, today", "=1-(1-Loan!B9)^(1/4)"
    heads = ["Quarter", "Owed at the start", "Interest", "Paid", "Owed at the end", "", "Worth today"]
    for c, head in enumerate(heads, start=1):
        ws.cell(4, c, head).font = Font(bold=True)
    for name, start, _, _ in PATHS:
        ws.cell(3, start, name).font = Font(bold=True)
        for k, head in enumerate(["Property", "Loan to value", "Chance of default if still paying",
                                  "Still paying", "Property when sold, if it defaults",
                                  "Share lost on default", "Chance of default", "Loss expected"]):
            ws.cell(4, start + k, head).font = Font(bold=True)
    for t in range(1, QUARTERS + 1):
        r = FIRST + t - 1
        ws.cell(r, 1, t)
        ws.cell(r, 2, "=Loan!$B$4" if t == 1 else f"=E{r - 1}")
        ws.cell(r, 3, f"=ROUND(B{r}*Loan!$B$5/4,2)")
        # The last payment clears what is left.
        ws.cell(r, 4, f"=IF(A{r}>Loan!$B$6,0,IF(A{r}=Loan!$B$6,B{r}+C{r},MIN(B{r}+C{r},$B$2)))")
        ws.cell(r, 5, f"=B{r}+C{r}-D{r}")
        ws.cell(r, 7, f"=1/(1+Loan!$B$5/4)^A{r}")
        for name, start, u, h in PATHS:
            if changed and name == "Better" and t == 57:
                u = "B"
            prop, ltv, hazard, alive, sold, lost, chance, loss = (col(start + k) for k in range(8))
            ws.cell(r, start, f"=Loan!$B$7*Economy!{h}{r}")
            ws.cell(r, start + 1, f"=$B{r}/{prop}{r}")
            ws.cell(r, start + 2,
                    f"=MIN(1,$B$3*EXP(Model!$B$11*(Economy!{u}{r}-Model!$B$10)"
                    f"+Model!$B$13*MAX(0,{ltv}{r}-Model!$B$12)))")
            ws.cell(r, start + 3, 1 if t == 1 else f"={alive}{r - 1}*(1-{hazard}{r - 1})")
            # A property is sold some quarters after the default, at the
            # prices of that quarter on this path.
            ws.cell(r, start + 4,
                    f"=Loan!$B$7*INDEX(Economy!{h}${FIRST}:{h}${LAST + AFTER},$A{r}+Model!$B$14)")
            ws.cell(r, start + 5,
                    f"=IF($B{r}<=0,0,MAX(Model!$B$16,MIN(1,1-{sold}{r}"
                    f"*(1-VLOOKUP({ltv}{r},Model!$D$5:$E$10,2,TRUE))*(1-Model!$B$15)/$B{r})))")
            ws.cell(r, start + 6, f"=IF($A{r}>Loan!$B$6,0,{alive}{r}*{hazard}{r})")
            ws.cell(r, start + 7, f"={chance}{r}*{lost}{r}*$B{r}*$G{r}")


def loan(ws):
    """The loan, and what to set aside for it."""
    ws["A1"] = "Expected credit loss on one mortgage"
    ws["A3"] = "The loan"
    for r, (what, value) in enumerate([
        ("Owed", 185000),
        ("Yearly interest rate", 0.0475),
        ("Quarters left", 88),
        ("Property value", 260000),
        ("Chance of default in a year, as scored when the loan was made", 0.008),
        ("Chance of default in a year, as scored today", 0.012),
        ("Days past due", 0),
    ], start=4):
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)
    ws["A12"] = "What to set aside"
    ws["A13"] = "Stage (1 sound, 2 looking shakier, 3 in default)"
    ws["B13"] = ("=IF(B10>=Model!B21,3,IF(OR(B10>Model!B20,"
                 "AND(B17>Model!B18*B18,B17-B18>=Model!B19)),2,1))")
    ws["A14"], ws["B14"] = "Provision", "=CHOOSE(B13,B15,B16,B19)"
    ws["A15"], ws["B15"] = "Loss expected over the next year", "=SUMPRODUCT(E13:E15,F13:F15)"
    ws["A16"], ws["B16"] = "Loss expected over the loan's life", "=SUMPRODUCT(E13:E15,G13:G15)"
    ws["A17"], ws["B17"] = "Chance of default over the loan's life", "=SUMPRODUCT(E13:E15,H13:H15)"
    ws["A18"], ws["B18"] = "The same, as expected when the loan was made", "=1-(1-B8)^(B6/4)"
    ws["A19"], ws["B19"] = "Loss expected if it is in default", "=B4*SUMPRODUCT(E13:E15,I13:I15)"
    for c, head in enumerate(["Path", "How likely", "Loss in a year", "Loss over its life",
                              "Chance of default over its life", "Share lost on default now"],
                             start=4):
        ws.cell(12, c, head).font = Font(bold=True)
    for r, (name, start, _, _) in enumerate(PATHS, start=13):
        chance, loss, lost = col(start + 6), col(start + 7), col(start + 5)
        ws.cell(r, 4, name)
        ws.cell(r, 5, f"=Model!B{r - 9}")
        ws.cell(r, 6, f"=SUM(Schedule!{loss}{FIRST}:{loss}{FIRST + 3})")
        ws.cell(r, 7, f"=SUM(Schedule!{loss}{FIRST}:{loss}{LAST})")
        ws.cell(r, 8, f"=SUM(Schedule!{chance}{FIRST}:{chance}{LAST})")
        ws.cell(r, 9, f"=Schedule!{lost}{FIRST}")
    for cell in ("A1", "A3", "A12"):
        ws[cell].font = Font(bold=True)


def write(path, changed=False):
    wb = openpyxl.Workbook()
    loan_sheet = wb.active
    loan_sheet.title = "Loan"
    loan(loan_sheet)
    schedule(wb.create_sheet("Schedule"), changed)
    economy(wb.create_sheet("Economy"))
    model(wb.create_sheet("Model"), changed)
    for ws in wb:
        ws.column_dimensions["A"].width = 58 if ws.title in ("Loan", "Model") else 12
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


def made_up_loan(rng):
    """One loan of a made-up book, of the kind `credit_loss.rs` makes."""
    owed = round(40000 * math.exp(2.7 * rng.unit()), 2)
    rate = round(0.02 + 0.05 * rng.unit(), 4)
    quarters = float(8 + int(93 * rng.unit()))
    value = round(owed / (0.2 + 0.9 * rng.unit() ** 0.7), 0)
    at_first = round(0.002 * math.exp(2.7 * rng.unit()), 5)
    drift = rng.unit()
    # Most loans are scored about as they were; one in seven much worse.
    now = at_first * (0.6 + 0.8 * rng.unit() if drift < 0.85 else 2.0 + 6.0 * rng.unit())
    now = round(min(now, 0.6), 5)
    late = rng.unit()
    if late < 0.95:
        days = 0.0
    elif late < 0.975:
        days = float(1 + int(30 * rng.unit()))
    elif late < 0.99:
        days = float(31 + int(59 * rng.unit()))
    else:
        days = float(90 + int(200 * rng.unit()))
    return [owed, rate, quarters, value, at_first, now, days]


def reference(workbook, out):
    """Drives LibreOffice through some loans, one at a time."""
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
        sheet = doc.Sheets.getByName("Loan")
        rng = Rng(0x9E3779B97F4A7C15)
        loans = [made_up_loan(rng) for _ in range(REFERENCE_LOANS)]
        rows = []
        started = time.perf_counter()
        for loan in loans:
            for cell, value in zip(INPUTS, loan):
                sheet.getCellRangeByName(cell).Value = value
            doc.calculateAll()
            rows.append(loan + [sheet.getCellRangeByName(c).Value for c in OUTPUTS])
        each = (time.perf_counter() - started) / len(loans)
        doc.close(True)
        with open(out, "w", newline="") as f:
            f.write(f"# LibreOffice recalculating credit_loss.xlsx for each loan: "
                    f"{each * 1000:.1f} ms a loan\n")
            w = csv.writer(f)
            w.writerow(["owed", "rate", "quarters", "value", "pd_at_first", "pd_now", "days_past_due",
                        "stage", "provision", "loss_in_a_year", "loss_over_its_life"])
            for row in rows:
                w.writerow([repr(float(x)) for x in row])
        print(f"LibreOffice: {each * 1000:.1f} ms a loan", file=sys.stderr)
    finally:
        office.terminate()


def main():
    target = os.path.join(HERE, "credit_loss.xlsx")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "credit_loss.xlsx")
        write(path)
        worked_out(path, HERE)
        path = os.path.join(tmp, "credit_loss_changed.xlsx")
        write(path, changed=True)
        worked_out(path, HERE)
    print("wrote", target, file=sys.stderr)
    if "--no-reference" not in sys.argv:
        reference(target, os.path.join(HERE, "credit_loss_reference.csv"))


if __name__ == "__main__":
    main()
