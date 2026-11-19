"""Writes `rating.xlsx`, the workbook the `quotes` example reads, and
`rating_reference.csv`, what LibreOffice makes of it for some customers.

The workbook is the kind a pricing team keeps: one customer and the cover
they ask for on the `Quote` sheet, and the tables of rates on `Rates`. It
prices a year's car insurance the usual way: a base rate times a factor for
each thing known about the driver and the car, less a discount for years
without a claim, a loading for a young driver in a fast car, a least
premium, a fee and tax, each to the penny. Every number in it is made up.

It is written with `openpyxl` and worked out by LibreOffice, which must both
be installed:

    python3 crates/watershed-gpu/examples/shared/rating.py

Then LibreOffice is driven directly: the customer's cells are set to each of
some made-up customers in turn and the workbook recalculated, as a macro
filling a table of quotes would. What it answers, and how long a quote took,
go in the reference file for the example to check itself against.
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
REFERENCE_QUOTES = 300

# The customer's cells, in the order the example and the reference file use.
INPUTS = ["B4", "B5", "B6", "B7", "B8", "B9", "B10"]
# What is read back: the year's premium, a monthly instalment, and whether
# the customer can be quoted at all.
OUTPUTS = ["B27", "B30", "B28"]

# What a customer can ask for; `quotes.rs` has the same lists.
AREAS = range(1, 11)
GROUPS = range(1, 51)
MILES = [3000, 6000, 9000, 13000, 20000]
EXCESSES = [0, 100, 250, 500, 1000]
COVERS = [1, 2]


def rates(ws):
    ws["A1"] = "Rates"
    ws["A1"].font = Font(bold=True)
    for r, (what, value) in enumerate([
        ("Base rate for a year", 640.0),
        ("Third party, fire and theft, as a share of comprehensive", 0.64),
        ("Least premium", 150.0),
        ("Fee", 25.0),
    ], start=3):
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)

    def table(col, heads, rows):
        for c, head in enumerate(heads):
            ws.cell(7, col + c, head).font = Font(bold=True)
        for r, row in enumerate(rows, start=8):
            for c, value in enumerate(row):
                ws.cell(r, col + c, value)

    # Age: from this age on. Dearest at 17, cheapest in the fifties.
    table(1, ["Age from", "Factor"], [
        (17, 3.60), (18, 3.10), (19, 2.70), (20, 2.35), (21, 2.05), (23, 1.72),
        (25, 1.38), (30, 1.12), (40, 0.96), (50, 0.88), (60, 0.92), (70, 1.08),
        (75, 1.30), (80, 1.62)])
    table(4, ["Area", "Factor"], [
        (a, f) for a, f in zip(AREAS, [0.82, 0.88, 0.93, 0.97, 1.00, 1.06, 1.13, 1.22, 1.34, 1.52])])
    # Vehicle groups 1 to 50: dearer by about 2.6% a group.
    table(7, ["Vehicle group", "Factor"], [(g, round(0.62 * 1.026 ** (g - 1), 3)) for g in GROUPS])
    table(10, ["Years without a claim", "Discount"], [
        (y, d) for y, d in enumerate([0, 0.20, 0.30, 0.38, 0.45, 0.50, 0.55, 0.58, 0.60, 0.62])])
    table(13, ["Miles a year from", "Factor"], [
        (0, 0.84), (5000, 0.93), (8000, 1.00), (12000, 1.09), (18000, 1.24)])
    table(16, ["Voluntary excess", "Factor"], [
        (e, f) for e, f in zip(EXCESSES, [1.00, 0.97, 0.93, 0.88, 0.80])])
    # A young driver in a fast car pays a loading: age from, by group from.
    ws["S6"] = "A young driver's loading, by vehicle group from"
    ws["S6"].font = Font(bold=True)
    ws["S7"] = "Age from"
    for c, group in enumerate([1, 11, 21, 31, 41], start=20):
        ws.cell(7, c, group)
    for r, (age, row) in enumerate([
        (17, [0, 60, 180, 420, 900]), (21, [0, 0, 90, 240, 520]),
        (25, [0, 0, 0, 60, 150]), (30, [0, 0, 0, 0, 0])], start=8):
        ws.cell(r, 19, age)
        for c, loading in enumerate(row, start=20):
            ws.cell(r, c, loading)
    ws["A24"], ws["B24"] = "Insurance premium tax", 0.12
    ws["A25"], ws["B25"] = "Yearly rate charged on instalments", 0.199
    ws["A26"], ws["B26"] = "Deposit, as a share", 0.2
    ws["A27"], ws["B27"] = "Not quoted: under this age", 21
    ws["A28"], ws["B28"] = "in a vehicle group above", 40
    ws.column_dimensions["A"].width = 52


def quote(ws):
    ws["A1"] = "A year's car insurance for one customer"
    ws["A3"] = "The customer"
    for r, (what, value) in enumerate([
        ("Age", 34),
        ("Area (1 to 10)", 4),
        ("Vehicle group (1 to 50)", 18),
        ("Years without a claim (0 to 9)", 5),
        ("Miles a year", 9000),
        ("Voluntary excess", 250),
        ("Cover (1 comprehensive, 2 third party, fire and theft)", 1),
    ], start=4):
        ws.cell(r, 1, what)
        ws.cell(r, 2, value)
    ws["A12"] = "Working"
    for r, (what, formula) in enumerate([
        ("Base rate", "=Rates!B3"),
        ("Age", "=VLOOKUP(B4,Rates!A8:B21,2,TRUE)"),
        ("Area", "=VLOOKUP(B5,Rates!D8:E17,2,FALSE)"),
        ("Vehicle group", "=INDEX(Rates!H8:H57,B6)"),
        ("Discount for years without a claim", "=INDEX(Rates!K8:K17,B7+1)"),
        ("Miles a year", "=INDEX(Rates!N8:N12,MATCH(B8,Rates!M8:M12,1))"),
        ("Voluntary excess", "=VLOOKUP(B9,Rates!P8:Q12,2,FALSE)"),
        ("Cover", "=CHOOSE(B10,1,Rates!B4)"),
        ("A young driver's loading",
         "=INDEX(Rates!T8:X11,MATCH(B4,Rates!S8:S11,1),MATCH(B6,Rates!T7:X7,1))"),
        ("For the risk", "=B13*B14*B15*B16*(1-B17)*B18*B19*B20+B21"),
        ("Or the least premium", "=MAX(B22,Rates!B5)"),
        ("With the fee", "=ROUND(B23+Rates!B6,2)"),
        ("Tax", "=ROUND(B24*Rates!B24,2)"),
    ], start=13):
        ws.cell(r, 1, what)
        ws.cell(r, 2, formula)
    ws["A27"], ws["B27"] = "Premium for the year", "=B24+B25"
    ws["A28"], ws["B28"] = "Can be quoted (1) or not (0)", "=IF(AND(B4<Rates!B27,B6>Rates!B28),0,1)"
    ws["A29"], ws["B29"] = "Paid monthly: deposit", "=ROUND(B27*Rates!B26,2)"
    ws["A30"], ws["B30"] = "and each of ten instalments", "=ROUND(PMT(Rates!B25/12,10,-(B27-B29)),2)"
    for cell in ("A1", "A3", "A12", "A27"):
        ws[cell].font = Font(bold=True)
    ws.column_dimensions["A"].width = 52


def write(path):
    wb = openpyxl.Workbook()
    sheet = wb.active
    sheet.title = "Quote"
    quote(sheet)
    rates(wb.create_sheet("Rates"))
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

    def pick(self, among):
        among = list(among)
        return float(among[int(self.unit() * len(among))])


def made_up_customer(rng):
    return [rng.pick(range(17, 81)), rng.pick(AREAS), rng.pick(GROUPS), rng.pick(range(10)),
            rng.pick(MILES), rng.pick(EXCESSES), rng.pick(COVERS)]


def reference(workbook, out):
    """Drives LibreOffice through some customers, one at a time."""
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
        sheet = doc.Sheets.getByName("Quote")
        rng = Rng(0x9E3779B97F4A7C15)
        customers = [made_up_customer(rng) for _ in range(REFERENCE_QUOTES)]
        rows = []
        started = time.perf_counter()
        for customer in customers:
            for cell, value in zip(INPUTS, customer):
                sheet.getCellRangeByName(cell).Value = value
            doc.calculateAll()
            rows.append(customer + [sheet.getCellRangeByName(c).Value for c in OUTPUTS])
        each = (time.perf_counter() - started) / len(customers)
        doc.close(True)
        with open(out, "w", newline="") as f:
            f.write(f"# LibreOffice recalculating rating.xlsx for each customer: "
                    f"{each * 1000:.2f} ms a quote\n")
            w = csv.writer(f)
            w.writerow(["age", "area", "group", "years", "miles", "excess", "cover",
                        "premium", "instalment", "quoted"])
            for row in rows:
                w.writerow([repr(float(x)) for x in row])
        print(f"LibreOffice: {each * 1000:.2f} ms a quote", file=sys.stderr)
    finally:
        office.terminate()


def main():
    target = os.path.join(HERE, "rating.xlsx")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "rating.xlsx")
        write(path)
        worked_out(path, HERE)
    print("wrote", target, file=sys.stderr)
    if "--no-reference" not in sys.argv:
        reference(target, os.path.join(HERE, "rating_reference.csv"))


if __name__ == "__main__":
    main()
