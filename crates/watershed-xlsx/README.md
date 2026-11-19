# watershed-xlsx

Turns the formulas of an Excel workbook into a watershed graph.

Someone has built a model in Excel. Some of its inputs are guesses. Name the
cells to vary and the cells to read, and this translates the formulas
between them into a graph with an input for each cell to vary. The graph can
then be changed and worked out again like any other, or handed to
[watershed-gpu](../watershed-gpu/README.md) to run for a million sets of
inputs at once.

```rust
use watershed_xlsx::{Model, Workbook};

let book = Workbook::open("model.xlsx")?;
let growth = book.cell("'Input sheet'!B26")?;
let value = book.cell("'Valuation output'!B33")?;
let mut model = Model::build(&book, &[growth], &[value])?;
// Every translated cell, against what Excel saved for it.
assert!(model.check().is_empty());
model.set(&[0.05])?;
println!("{}", model.value(value).unwrap());

// The same, for many sets of inputs at once.
let plan = watershed_gpu::Scenarios::new(model.graph(), &model.inputs(), &model.outputs())?;
```

The `spreadsheet` example in watershed-gpu does this for a valuation
workbook and a million trials, and `credit_loss` for a workbook that works
out one loan's expected credit loss, run for a whole loan book. `audit`
tries a workbook over the whole of its inputs' ranges, and against a later
version of itself. `sensitivities` asks the graph for an answer's
derivative by hundreds of the workbook's cells at once. `quotes` works out
every price a rating workbook can give.

## What is translated

Only what lies between the inputs and the outputs. A cell that does not
depend on a varied input is taken as the value Excel saved for it, whatever
its formula. Where a condition does not depend on one (`IF(B17="Yes", ..)`),
only the branch it takes is followed. So a workbook can be full of things
this does not understand, as long as they are not in the way. Anything in
the way that it does not understand is refused, with the cell's name and
the reason. Nothing is guessed.

Formulas can use numbers, text, TRUE and FALSE, Excel's errors (`#N/A`),
`+ - * / ^ % &`, comparisons, ranges (`B2:B9`, whole columns such as `B:B`,
on any sheet), defined names, and shared formulas (a formula filled down,
which Excel saves once). These functions are understood whether or not what
they are given depends on an input:

`ABS` `AND` `AVERAGE` `CHOOSE` `EXP` `HLOOKUP` `IF` `IFERROR` `INDEX` `INT`
`LN` `LOG` `LOG10` `MATCH` `MAX` `MIN` `NOT` `NPV` `OR` `PMT` `POWER` `ROUND`
`ROUNDDOWN` `ROUNDUP` `SIGN` `SQRT` `SUM` `SUMPRODUCT` `VLOOKUP`

A lookup by a value that depends on an input becomes a chain of
conditions, one for each row of the table. These are understood only when
what they are given does not depend on an input:

`COUNT` `COUNTA` `MOD`

The number of places given to `ROUND`, `ROUNDUP` and `ROUNDDOWN` must not
depend on an input either.

Cells that read each other in a loop (a circular reference, worked out by
Excel's iterative calculation) are fine as long as the formulas followed do
not go round the loop. Valuation workbooks often have one behind a switch
that is off. A loop that is followed is refused.

## Where it differs from Excel

- An Excel error that comes from a varied input (dividing by zero, say) is
  infinity or not-a-number here. `IFERROR` treats both as errors, and
  `Model::check` counts either as agreeing with an error in Excel.
- Text cannot come from a varied input. `IF(x > 0, x, "")` is refused, as is
  joining text to a number with `&`.
- `0^0` is 1 here when it comes from a varied input, where Excel has
  `#NUM!`.
- An `INDEX` or `CHOOSE` position that depends on an input and comes to
  less than 1 is not-a-number here. Excel takes 0 in `INDEX` to mean the
  whole row or column.
- Approximate lookups (`VLOOKUP` with TRUE, `MATCH` with 1 or -1) need their
  keys in order, as Excel asks. A table whose keys are not in order is
  refused, since Excel's answer then depends on how it searches.
- Excel rounds a number as it reads to fifteen figures, so that `0.29 * 100`,
  which is held as 28.999999999999996, rounds down to 29 and `1.005` rounds
  to 1.01. For a number that depends on an input, `ROUND`, `ROUNDUP`,
  `ROUNDDOWN` and `INT` do this by taking a number within four parts in
  10¹⁶ of a half or a whole number as that half or whole number. Fifteen
  figures is a little wider: up to five parts in 10¹⁵. A number in the gap
  between the two can round the other way here. None of the test workbook's
  does. To no places, `ROUND` takes a half as a half exactly, as LibreOffice
  does. Above 2⁵² once multiplied up, a rounded number may be out in its
  last figure.
- `.xls` files are not read: their formulas are read incompletely, and a
  cell whose formula is missing would be taken for a constant.

## How it is checked

Excel saves the last value it worked out for every cell, so a workbook is
its own check: `Model::check` compares each translated cell with it.

The tests use [`tests/workbooks/functions.xlsx`](tests/workbooks/functions.xlsx),
written by [`make.py`](tests/workbooks/make.py) and worked out by
LibreOffice. Its 80-odd formulas cover every function above. Each row
repeats them for a different pair of inputs: 24 rows, which take each
condition both ways and reach errors, zeros and negative numbers. The tests
translate the first row and set its inputs to each row's in turn. Every
value must agree with LibreOffice's to twelve figures, on the graph and as
a plan. A second sheet, `Rounding`, does the same for sixteen formulas that
round something depending on an input, over 396 pairs of inputs: halves at
the third place, products that are a hair under a half or a whole number,
both signs, and 300 amounts in pence at eight rates. Other tests write small
workbooks themselves: where LibreOffice and Excel differ (LibreOffice keeps
TRUE as 1, and so adds it up in a range),
for shared formulas, and for a running total 200,000 cells long. A further
sheet holds formulas that must be refused, each with the reason it must
give. Changing the lookups, `IFERROR`, `CHOOSE`, `NPV`, how text is compared,
how loops are found or how a number is rounded makes a test fail.

Damodaran's `fcffsimpleginzu.xlsx` was also tried, by hand, not in CI.
Varying each of its 15 inputs that reach the valuation, every translated
cell agreed with what Excel saved. With each input then raised by a tenth,
the valuation came to what LibreOffice got recalculating the workbook, to
the 15 figures it saves.

To write the test workbook again, with `openpyxl` and LibreOffice installed:

```bash
python3 crates/watershed-xlsx/tests/workbooks/make.py
```
