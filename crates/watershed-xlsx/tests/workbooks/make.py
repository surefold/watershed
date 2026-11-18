"""Writes the workbooks the tests read, and has LibreOffice work them out.

    python3 crates/watershed-xlsx/tests/workbooks/make.py

Needs `openpyxl` and LibreOffice (`soffice`). The formulas are written here
with no values; LibreOffice works every cell out and saves the workbook
again, so the values the tests compare with are a spreadsheet program's own,
not this script's.

`functions.xlsx`:

- `Cases`: columns A and B are the inputs, one pair a row. Every other
  column holds one formula, the same in every row but for the row it reads,
  with what it tries in row 1. The tests translate row 2's formula, then set
  the inputs to each row's in turn and compare with that row's value.
- `Refused`: formulas that must be refused, with a word the reason must
  contain in row 1.
- `Tables` and `Rates & fees`: what the formulas read that does not vary.

LibreOffice keeps TRUE and FALSE as the numbers 1 and 0, so where Excel and
it differ over them (a TRUE in a range given to `SUM` is passed over by
Excel and added by LibreOffice) the tests write their own small workbooks
instead.
"""

import os
import shutil
import subprocess
import sys
import tempfile

import openpyxl
from openpyxl.workbook.defined_name import DefinedName

HERE = os.path.dirname(os.path.abspath(__file__))

XS = [-3, -1, -0.5, 0, 0.25, 1, 1.5, 2, 2.5, 3, 4, 5, 7, 10, 12.5, 20, 35, 50,
      99.9, 100, 150, 1e-9, 2.675, 64]
YS = [2, 0, 1, -2, 0.5, 3, 10, -0.25, 4, 1, 0, 7, 2.5, -1, 0.1, 1, 5, 2, 0,
      3.5, -4, 1, 8, 0.5]

CASES = [
    # Arithmetic.
    ("arithmetic", "=A{r}*2+B{r}/4-1"),
    ("square", "=A{r}^2"),
    ("minus binds tighter than ^", "=-A{r}^2"),
    ("powers of two", "=2^A{r}"),
    ("power", "=A{r}^B{r}"),
    ("divide", "=A{r}/B{r}"),
    ("percent", "=A{r}%*B{r}"),
    ("brackets", "=(A{r}+B{r})*(A{r}-B{r})"),
    ("left to right", "=A{r}-B{r}-1"),
    ("text that is a number", '=A{r}+"2"'),
    # Comparisons and IF.
    ("greater", "=IF(A{r}>B{r},1,0)"),
    ("equal", "=IF(A{r}=B{r},1,0)"),
    ("not equal", "=IF(A{r}<>2,A{r},-1)"),
    ("nested IF", "=IF(A{r}>=1,IF(A{r}<=3,1,2),0)"),
    ("a number is below text", '=IF(A{r}<"a",A{r},0)'),
    ("IF both ways", "=IF(A{r}>0,A{r}*B{r},B{r}-A{r})"),
    ("IF on a fixed condition", '=IF(Tables!$B$1="Yes",A{r}*2,A{r}*3)'),
    ("text compared whatever its case", '=IF(Tables!$B$1="YES",A{r}*2,A{r}*3)'),
    ("text looked up whatever its case", '=IF(VLOOKUP("yes",Tables!$B$1:$C$1,1,FALSE)="Yes",A{r},0)'),
    ("a number as a condition", "=IF(A{r},1,2)"),
    ("IF with its value left out", "=IF(A{r}>0,,B{r})"),
    ("true and false as numbers", "=IF(A{r}>0,TRUE,FALSE)*5"),
    ("an error written in", "=IF(A{r}>100,#N/A,A{r})"),
    ("text joined, fixed", '=IF(Tables!$B$1&"!"="Yes!",A{r},0)'),
    # IFERROR.
    ("IFERROR of a division", "=IFERROR(1/A{r},-1)"),
    ("IFERROR of LN", "=IFERROR(LN(A{r}),0)"),
    ("IFERROR of a square root", "=IFERROR(SQRT(A{r})*B{r},B{r})"),
    # AND, OR, NOT.
    ("AND", "=IF(AND(A{r}>0,B{r}>0),1,0)"),
    ("OR", "=IF(OR(A{r}>5,B{r}<0),1,0)"),
    ("NOT", "=IF(NOT(A{r}>1),A{r},0)"),
    ("AND of a range of TRUE", "=IF(AND(Tables!$E$2:$E$4,A{r}>0),1,0)"),
    ("OR settled by a constant", "=IF(OR(A{r}>1,B{r}>1,TRUE),A{r},0)"),
    ("AND settled by a constant", "=IF(AND(A{r}>1,FALSE),0,A{r})"),
    # SUM, AVERAGE, MIN, MAX.
    ("SUM", "=SUM(A{r},B{r},1)"),
    ("SUM of a range", "=SUM(A{r}:B{r})"),
    ("SUM of a fixed range", "=SUM(Tables!$A$2:$A$6)*A{r}"),
    ("AVERAGE passes over text", "=AVERAGE(A{r},B{r},Tables!$C$2:$C$6)"),
    ("MIN", "=MIN(A{r},B{r},2)"),
    ("MAX", "=MAX(A{r},B{r}*2)"),
    ("MIN with one left out", "=MIN(A{r},)"),
    # The rest of the arithmetic.
    ("ABS", "=ABS(A{r}-2)"),
    ("EXP", "=EXP(A{r}/10)"),
    ("LN", "=LN(A{r})"),
    ("SQRT", "=SQRT(A{r})"),
    ("LOG10", "=LOG10(A{r})"),
    ("LOG base 2", "=LOG(A{r},2)"),
    ("LOG base 10", "=LOG(A{r})"),
    ("SIGN", "=SIGN(A{r}-B{r})"),
    ("POWER", "=POWER(A{r},2)+POWER(2,B{r})"),
    # Money.
    ("NPV", "=NPV(A{r}/100,100,200,300)"),
    ("NPV of a range", "=NPV(0.05,A{r},B{r},Tables!$A$2:$A$4)"),
    ("PMT", "=PMT(A{r}/100,10,1000)"),
    ("PMT due at the start", "=PMT(0.05,12,A{r}*1000,B{r},1)"),
    ("PMT with no interest", "=PMT(A{r}/1200,360,250000,0,0)"),
    ("SUMPRODUCT", "=SUMPRODUCT(A{r}:B{r},Tables!$A$2:$B$2)"),
    # Choosing.
    ("CHOOSE by a fixed number", "=CHOOSE(Tables!$B$3,A{r},B{r},3)"),
    ("CHOOSE by an input", "=CHOOSE(A{r},10,20,B{r})"),
    # Lookups.
    ("VLOOKUP nearest", "=VLOOKUP(A{r},Tables!$G$2:$H$10,2)"),
    ("VLOOKUP exact", "=VLOOKUP(A{r},Tables!$G$2:$H$6,2,FALSE)"),
    ("VLOOKUP of a fixed key", "=VLOOKUP(Tables!$B$4,Tables!$G$2:$H$6,2,FALSE)*A{r}"),
    ("VLOOKUP of the key column", "=VLOOKUP(B{r},Tables!$G$2:$H$6,1)"),
    ("HLOOKUP", "=HLOOKUP(A{r},Tables!$G$12:$K$13,2)"),
    ("MATCH nearest", "=MATCH(A{r},Tables!$G$2:$G$6)"),
    ("MATCH exact", "=MATCH(A{r},Tables!$G$2:$G$6,0)"),
    ("MATCH from the top", "=MATCH(A{r},Tables!$J$2:$J$6,-1)"),
    ("INDEX and MATCH", "=INDEX(Tables!$H$2:$H$6,MATCH(A{r},Tables!$G$2:$G$6))"),
    ("INDEX by an input", "=INDEX(Tables!$G$2:$H$6,ABS(A{r})+1,2)"),
    ("INDEX by two", "=INDEX(Tables!$G$2:$H$6,ABS(A{r})+1,ABS(B{r})+1)"),
    ("INDEX along a row", "=INDEX(Tables!$A$2:$B$2,A{r})"),
    ("INDEX of a fixed place", "=INDEX(Tables!$G$2:$H$6,3,2)*A{r}"),
    # Functions of what is fixed.
    ("ROUND", "=ROUND(Tables!$A$7,2)*A{r}"),
    ("ROUNDUP", "=ROUNDUP(Tables!$A$7,1)+A{r}"),
    ("ROUNDDOWN", "=ROUNDDOWN(Tables!$A$8,-1)+A{r}"),
    ("INT", "=INT(Tables!$A$8)*A{r}"),
    ("MOD", "=MOD(Tables!$A$8,3)+A{r}"),
    ("COUNT", "=COUNT(Tables!$C$2:$C$6,A{r})+A{r}"),
    ("COUNTA", "=COUNTA(Tables!$C$2:$C$6)+A{r}"),
    # Names and places.
    ("a defined name", "=A{r}*Rate"),
    ("a defined range", "=VLOOKUP(A{r},Bands,2)"),
    ("a sheet in quotes", "='Rates & fees'!$B$1*A{r}"),
    ("a whole column", "=SUM(Tables!M:M)*A{r}"),
    ("another formula's cell", "=C{r}+D{r}"),
]

REFUSED = [
    ("text", '=IF(A{r}>0,A{r},"")'),
    ("joined", '=A{r}&"x"'),
    ("ROUND", "=ROUND(A{r},1)"),
    ("not understood", "=OFFSET(A{r},0,0)"),
    ("keys depend", "=VLOOKUP(1,A{r}:B{r},2,FALSE)"),
    ("not in order", "=VLOOKUP(A{r},Tables!$O$2:$P$4,2)"),
    ("true or false the other", "=IF(A{r}>0,1,TRUE)"),
    ("itself", "=A{r}+E{r}"),
    ("itself", "=D{r}*2"),
]


def tables(ws):
    for r, x in enumerate([1, 2, 3, 4, 5], start=2):
        ws.cell(r, 1, x)
    ws["A7"] = 2.675
    ws["A8"] = 17.9
    ws["B1"] = "Yes"
    ws["B2"] = 0.5
    ws["B3"] = 2
    ws["B4"] = 20
    ws["B5"] = 0.07
    for r, v in enumerate([3, "text", None, "7", 4], start=2):
        if v is not None:
            ws.cell(r, 3, v)
    for r in range(2, 5):
        ws.cell(r, 5, True)
    for r, (k, v) in enumerate([(0, 0), (10, 0.1), (20, 0.2), (50, 0.4), (100, 0.45)], start=2):
        ws.cell(r, 7, k)
        ws.cell(r, 8, v)
    for r, k in enumerate([100, 50, 20, 10, 0], start=2):
        ws.cell(r, 10, k)
    for c, (k, v) in enumerate([(0, 5), (1, 6), (2, 7), (5, 8), (10, 9)], start=7):
        ws.cell(12, c, k)
        ws.cell(13, c, v)
    for r, v in enumerate([1, 2, 3], start=1):
        ws.cell(r, 13, v)
    for r, (k, v) in enumerate([(5, 1), (1, 2), (3, 3)], start=2):
        ws.cell(r, 15, k)
        ws.cell(r, 16, v)


def letter(n):
    return openpyxl.utils.get_column_letter(n)


def functions(path):
    wb = openpyxl.Workbook()
    cases = wb.active
    cases.title = "Cases"
    cases["A1"], cases["B1"] = "x", "y"
    for r, (x, y) in enumerate(zip(XS, YS), start=2):
        cases.cell(r, 1, x)
        cases.cell(r, 2, y)
        for c, (_, formula) in enumerate(CASES, start=3):
            cases.cell(r, c, formula.format(r=r))
    for c, (what, _) in enumerate(CASES, start=3):
        cases.cell(1, c, what)

    refused = wb.create_sheet("Refused")
    refused["A1"], refused["A2"], refused["B2"] = "x", 1.5, 2
    for c, (why, formula) in enumerate(REFUSED, start=3):
        refused.cell(1, c, why)
        refused.cell(2, c, formula.format(r=2).replace("E2", letter(c + 1) + "2").replace("D2", letter(c - 1) + "2"))

    tables(wb.create_sheet("Tables"))
    fees = wb.create_sheet("Rates & fees")
    fees["B1"] = 1.25
    wb.defined_names["Rate"] = DefinedName("Rate", attr_text="Tables!$B$5")
    wb.defined_names["Bands"] = DefinedName("Bands", attr_text="Tables!$G$2:$H$6")
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


def main():
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "functions.xlsx")
        functions(path)
        worked_out(path, HERE)
    print("wrote", os.path.join(HERE, "functions.xlsx"), file=sys.stderr)


if __name__ == "__main__":
    main()
