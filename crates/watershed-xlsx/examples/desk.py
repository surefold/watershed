"""Writes `desk.xlsx`, the workbook the `live` example follows, and
`desk_reference.csv`, what LibreOffice makes of it as prices move.

The workbook is the kind a trading floor's risk desk keeps: the prices of
sixty shares on `Market`, six hundred positions in them on `Book`, each
desk's totals and limits on `Desks`, and the floor's on `Floor`. Every name
and number in it is made up.

It is written with `openpyxl` and worked out by LibreOffice, which must both
be installed:

    python3 crates/watershed-xlsx/examples/desk.py

Then LibreOffice is driven directly: one price is changed at a time, three
hundred times, and the workbook left to recalculate what that reaches, as a
sheet fed by a price feed does. The floor's figures after each change, and
how long a change took, go in the reference file for the example to check
itself against.
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
SHARES = 60
DESKS = 12
EACH = 50  # Positions a desk.
FIRST = 5  # The first row of each table.
TICKS = 300

# The floor's figures, in the order the example and the reference file use.
FLOOR = [f"B{r}" for r in range(4, 13)]


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


def share(k):
    return f"S{k + 1:02d}"


def write(path):
    rng = Rng(0x9E3779B97F4A7C15)
    wb = openpyxl.Workbook()

    market = wb.active
    market.title = "Market"
    market["A1"] = "Prices"
    for c, head in enumerate(["Share", "Price", "Yesterday's close"], start=1):
        market.cell(4, c, head).font = Font(bold=True)
    prices = []
    for k in range(SHARES):
        price = round(4 * math.exp(4 * rng.unit()), 2)
        prices.append(price)
        market.cell(FIRST + k, 1, share(k))
        market.cell(FIRST + k, 2, price)
        market.cell(FIRST + k, 3, price)
    last_share = FIRST + SHARES - 1
    table = f"Market!$A${FIRST}:$C${last_share}"

    book = wb.create_sheet("Book")
    book["A1"] = "Positions, a desk at a time"
    heads = ["Desk", "Share", "Shares held", "Bought at", "Stop at", "Price", "Value",
             "Size", "Profit", "Today", "Stopped"]
    for c, head in enumerate(heads, start=1):
        book.cell(4, c, head).font = Font(bold=True)
    for p in range(DESKS * EACH):
        r = FIRST + p
        k = int(rng.unit() * SHARES)
        # Mostly holdings, some sold short; bought within a tenth of today's price.
        short = rng.unit() < 0.25
        held = round((200 + 9800 * rng.unit() ** 2) * 40 / prices[k]) * (-1 if short else 1)
        bought = round(prices[k] * (0.9 + 0.2 * rng.unit()), 2)
        stop = round(prices[k] * ((1.06 + 0.1 * rng.unit()) if short else (0.94 - 0.1 * rng.unit())), 2)
        book.cell(r, 1, f"Desk {p // EACH + 1}")
        book.cell(r, 2, share(k))
        book.cell(r, 3, held)
        book.cell(r, 4, bought)
        book.cell(r, 5, stop)
        book.cell(r, 6, f"=VLOOKUP(B{r},{table},2,FALSE)")
        book.cell(r, 7, f"=C{r}*F{r}")
        book.cell(r, 8, f"=ABS(G{r})")
        book.cell(r, 9, f"=(F{r}-D{r})*C{r}")
        book.cell(r, 10, f"=(F{r}-VLOOKUP(B{r},{table},3,FALSE))*C{r}")
        # A holding is stopped when the price falls to its stop; a short, when it rises to it.
        book.cell(r, 11, f"=IF(C{r}>0,IF(F{r}<=E{r},1,0),IF(F{r}>=E{r},1,0))")

    desks = wb.create_sheet("Desks")
    desks["A1"] = "Each desk"
    heads = ["Desk", "Size", "Net", "Profit", "Today", "Limit on size", "Used", "Over its limit",
             "Positions stopped", "Most it may lose in a day", "Through that"]
    for c, head in enumerate(heads, start=1):
        desks.cell(4, c, head).font = Font(bold=True)
    for d in range(DESKS):
        r = FIRST + d
        a, b = FIRST + d * EACH, FIRST + (d + 1) * EACH - 1
        desks.cell(r, 1, f"Desk {d + 1}")
        desks.cell(r, 2, f"=SUM(Book!H{a}:H{b})")
        desks.cell(r, 3, f"=SUM(Book!G{a}:G{b})")
        desks.cell(r, 4, f"=SUM(Book!I{a}:I{b})")
        desks.cell(r, 5, f"=SUM(Book!J{a}:J{b})")
        # Limits are set a little above where each desk stands, filled in below.
        desks.cell(r, 7, f"=B{r}/F{r}")
        desks.cell(r, 8, f"=IF(B{r}>F{r},1,0)")
        desks.cell(r, 9, f"=SUM(Book!K{a}:K{b})")
        desks.cell(r, 11, f"=IF(E{r}<J{r},1,0)")
    last_desk = FIRST + DESKS - 1

    floor = wb.create_sheet("Floor")
    floor["A1"] = "The floor"
    for r, (what, formula) in enumerate([
        ("Net value", f"=SUM(Desks!C{FIRST}:C{last_desk})"),
        ("Size", f"=SUM(Desks!B{FIRST}:B{last_desk})"),
        ("Profit", f"=SUM(Desks!D{FIRST}:D{last_desk})"),
        ("Today", f"=SUM(Desks!E{FIRST}:E{last_desk})"),
        ("Desks over their limit", f"=SUM(Desks!H{FIRST}:H{last_desk})"),
        ("Desks through their loss for the day", f"=SUM(Desks!K{FIRST}:K{last_desk})"),
        ("Positions stopped", f"=SUM(Desks!I{FIRST}:I{last_desk})"),
        ("The fullest desk, as a share of its limit", f"=MAX(Desks!G{FIRST}:G{last_desk})"),
        ("Margin to post", "=ROUND(0.12*B5+MAX(0,-B7),2)"),
    ], start=4):
        floor.cell(r, 1, what)
        floor.cell(r, 2, formula)
    for ws, width in ((floor, 44), (desks, 10), (book, 10), (market, 10)):
        ws.column_dimensions["A"].width = width
    wb.save(path)
    return prices


def worked_out(path, out_dir):
    """Has LibreOffice work out every cell and save the workbook again."""
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(
            ["soffice", "--headless", "--calc", "--convert-to", "xlsx", "--outdir", tmp, path],
            check=True,
            capture_output=True,
        )
        shutil.move(os.path.join(tmp, os.path.basename(path)), os.path.join(out_dir, os.path.basename(path)))


def with_limits(path):
    """Sets each desk's limits from where it stands, once LibreOffice has said."""
    values = openpyxl.load_workbook(path, data_only=True)["Desks"]
    wb = openpyxl.load_workbook(path)
    desks = wb["Desks"]
    for d in range(DESKS):
        r = FIRST + d
        size = values.cell(r, 2).value
        # Room for a few percent more, and a day's loss of about one percent.
        desks.cell(r, 6, round(size * (1.015 + 0.01 * (d % 4)), -3))
        desks.cell(r, 10, -round(size * (0.006 + 0.002 * (d % 3)), -2))
    wb.save(path)


def reference(workbook, prices, out):
    """Drives LibreOffice through a run of prices, one change at a time."""
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
        market = doc.Sheets.getByName("Market")
        floor = doc.Sheets.getByName("Floor")
        rng = Rng(0x2545F4914F6CDD1D)
        prices = list(prices)
        rows = []
        started = time.perf_counter()
        for _ in range(TICKS):
            k = int(rng.unit() * SHARES)
            # A move of up to two percent either way, to the penny.
            prices[k] = round(prices[k] * (1 + 0.04 * (rng.unit() - 0.5)), 2)
            market.getCellRangeByName(f"B{FIRST + k}").Value = prices[k]
            doc.calculate()
            rows.append([k, prices[k]] + [floor.getCellRangeByName(c).Value for c in FLOOR])
        each = (time.perf_counter() - started) / TICKS
        doc.close(True)
        with open(out, "w", newline="") as f:
            f.write(f"# LibreOffice recalculating desk.xlsx after each price: "
                    f"{each * 1000:.2f} ms a price\n")
            w = csv.writer(f)
            w.writerow(["share", "price", "net", "size", "profit", "today", "over_limit",
                        "through_loss", "stopped", "fullest", "margin"])
            for row in rows:
                w.writerow([repr(float(x)) for x in row])
        print(f"LibreOffice: {each * 1000:.2f} ms a price", file=sys.stderr)
    finally:
        office.terminate()


def main():
    target = os.path.join(HERE, "desk.xlsx")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "desk.xlsx")
        prices = write(path)
        # Once for each desk's size, to set its limits by; then with them.
        first = os.path.join(tmp, "first")
        os.makedirs(first)
        worked_out(path, first)
        with_limits(os.path.join(first, "desk.xlsx"))
        worked_out(os.path.join(first, "desk.xlsx"), HERE)
    print("wrote", target, file=sys.stderr)
    if "--no-reference" not in sys.argv:
        reference(target, prices, os.path.join(HERE, "desk_reference.csv"))


if __name__ == "__main__":
    main()
