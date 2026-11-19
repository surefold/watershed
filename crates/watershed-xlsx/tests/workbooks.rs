//! Each function, against what a spreadsheet program worked out for it.
//!
//! `functions.xlsx` was written by `workbooks/make.py` and worked out by
//! LibreOffice. In its `Cases` sheet every row repeats the same formulas for
//! different inputs, so translating row 2 and setting its inputs to each
//! row's in turn must give each row's values.

use std::io::Write;

use watershed_gpu::Scenarios;
use watershed_xlsx::{Cell, Error, FIXED_FUNCTIONS, FUNCTIONS, Model, Val, Workbook};

fn functions() -> Workbook {
    Workbook::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/workbooks/functions.xlsx"
    ))
    .expect("the test workbook")
}

fn sheet(book: &Workbook, name: &str) -> usize {
    book.sheets().position(|s| s == name).expect("the sheet")
}

/// The same to about twelve figures, or both errors.
fn agrees(here: f64, excel: &Val) -> bool {
    match excel {
        Val::Num(x) => (here - x).abs() <= 1e-9 * here.abs().max(x.abs()).max(1e-6),
        Val::Bool(b) => here == f64::from(u8::from(*b)),
        Val::Empty => here == 0.0,
        Val::Error => !here.is_finite(),
        Val::Text(_) => false,
    }
}

#[test]
fn every_formula_gives_what_libreoffice_gave_for_every_row() {
    every_row_agrees("Cases", 20, 80);
}

/// Rounding what depends on an input, where it is hard: halves, numbers a
/// hair under a half or a whole number, and both signs.
#[test]
fn rounding_gives_what_libreoffice_gave_for_every_row() {
    every_row_agrees("Rounding", 350, 16);
}

/// Translates each formula in the sheet's second row, sets its inputs to
/// each row's in turn, and holds every value to what LibreOffice has there.
fn every_row_agrees(name: &str, least_rows: usize, least_cases: usize) {
    let book = functions();
    let cases = sheet(&book, name);
    let at = |row, col| Cell {
        sheet: cases,
        row,
        col,
    };
    let rows: Vec<(f64, f64)> = (1..)
        .map_while(
            |row| match (book.value(at(row, 0)), book.value(at(row, 1))) {
                (Val::Num(x), Val::Num(y)) => Some((x, y)),
                _ => None,
            },
        )
        .collect();
    assert!(rows.len() >= least_rows, "only {} rows", rows.len());
    let mut tried = 0;
    let mut wrong = Vec::new();
    for col in 2.. {
        let Val::Text(what) = book.value(at(0, col)) else {
            break;
        };
        tried += 1;
        let mut model = match Model::build(&book, &[at(1, 0), at(1, 1)], &[at(1, col)]) {
            Ok(model) => model,
            Err(e) => {
                wrong.push(format!("{what}: refused: {e}"));
                continue;
            }
        };
        for m in model.check() {
            wrong.push(format!("{what}: {m}"));
        }
        // Row by row on the graph.
        let mut on_graph = Vec::new();
        for (k, &(x, y)) in rows.iter().enumerate() {
            model.set(&[x, y]).unwrap();
            let here = model.value(at(1, col)).unwrap();
            let excel = book.value(at(k as u32 + 1, col));
            if !agrees(here, &excel) {
                wrong.push(format!(
                    "{what}: with x = {x} and y = {y}, {here} here and {excel:?} in LibreOffice"
                ));
            }
            on_graph.push(here);
        }
        // Every row at once, as a plan.
        let plan = Scenarios::new(model.graph(), &model.inputs(), &model.outputs()).unwrap();
        let inputs: Vec<f64> = rows.iter().flat_map(|&(x, y)| [x, y]).collect();
        for ((k, a), b) in plan.run_cpu(&inputs).into_iter().enumerate().zip(&on_graph) {
            if a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()) {
                wrong.push(format!(
                    "{what}: row {}: {a} as a plan, {b} on the graph",
                    k + 2
                ));
            }
        }
    }
    assert!(tried >= least_cases, "only {tried} cases");
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn each_function_understood_is_tried() {
    let book = functions();
    let cases = sheet(&book, "Cases");
    let mut formulas = String::new();
    for col in 2..200 {
        formulas.push_str(
            book.formula(Cell {
                sheet: cases,
                row: 1,
                col,
            })
            .unwrap_or(""),
        );
    }
    for f in FUNCTIONS.iter().chain(FIXED_FUNCTIONS) {
        assert!(formulas.contains(&format!("{f}(")), "{f} is not tried");
    }
}

#[test]
fn what_cannot_be_translated_is_refused_with_the_reason() {
    let book = functions();
    let refused = sheet(&book, "Refused");
    let at = |row, col| Cell {
        sheet: refused,
        row,
        col,
    };
    for col in 2.. {
        let Val::Text(why) = book.value(at(0, col)) else {
            break;
        };
        match Model::build(&book, &[at(1, 0), at(1, 1)], &[at(1, col)]) {
            Ok(_) => panic!(
                "{}: translated, but should be refused",
                book.name(at(1, col))
            ),
            Err(e) => assert!(
                e.to_string().contains(&why),
                "{}: refused for the wrong reason: {e}",
                book.name(at(1, col))
            ),
        }
    }
}

#[test]
fn cells_are_found_by_name() {
    let book = functions();
    let tables = sheet(&book, "Tables");
    let fees = sheet(&book, "Rates & fees");
    let cell = |sheet, row, col| Cell { sheet, row, col };
    assert_eq!(book.cell("Tables!B5").unwrap(), cell(tables, 4, 1));
    assert_eq!(book.cell("tables!$b$5").unwrap(), cell(tables, 4, 1));
    assert_eq!(book.cell("Rate").unwrap(), cell(tables, 4, 1));
    assert_eq!(book.cell("'Rates & fees'!B1").unwrap(), cell(fees, 0, 1));
    assert_eq!(book.cell("C3").unwrap(), cell(0, 2, 2));
    assert!(matches!(book.cell("Bands"), Err(Error::Name { .. })));
    assert!(matches!(book.cell("Nowhere!A1"), Err(Error::Name { .. })));
    assert!(matches!(book.cell("NoSuchName"), Err(Error::Name { .. })));
    // A name written out reads back as the same cell.
    for sheet in 0..book.sheets().count() {
        let c = cell(sheet, 41, 730);
        assert_eq!(book.cell(&book.name(c)).unwrap(), c);
    }
    assert_eq!(book.name(cell(fees, 0, 1)), "'Rates & fees'!B1");
}

#[test]
fn inputs_and_outputs_that_cannot_be_are_refused() {
    let book = functions();
    let cases = sheet(&book, "Cases");
    let at = |row, col| Cell {
        sheet: cases,
        row,
        col,
    };
    let refused = |vary: &[Cell], outputs: &[Cell], why: &str| {
        let e = Model::build(&book, vary, outputs).err().expect("refused");
        assert!(e.to_string().contains(why), "{e}");
    };
    // The header is text.
    refused(&[at(0, 0)], &[at(1, 2)], "does not hold a number");
    refused(&[at(1, 0), at(1, 0)], &[at(1, 2)], "twice");
    refused(&[at(1, 0)], &[at(1, 0)], "is an input");
    // Row 3's formulas do not read row 2's inputs.
    refused(&[at(1, 0)], &[at(2, 2)], "does not depend");
}

#[test]
fn a_graph_follows_only_the_branch_a_fixed_condition_takes() {
    // `Tables!B1` is "Yes", so only `A2*2` is followed: B2 never is.
    let book = functions();
    let cases = sheet(&book, "Cases");
    let col = (2..)
        .find(|&col| {
            book.value(Cell {
                sheet: cases,
                row: 0,
                col,
            }) == Val::Text("IF on a fixed condition".into())
        })
        .unwrap();
    let at = |row, col| Cell {
        sheet: cases,
        row,
        col,
    };
    let mut model = Model::build(&book, &[at(1, 0)], &[at(1, col)]).unwrap();
    assert!(model.check().is_empty());
    assert_eq!(model.translated(), 1);
    assert_eq!(model.folded(), 0);
}

// ---- Workbooks written here ----

/// A one-sheet workbook. Each cell is its address, then its value, then
/// its formula as the file holds it, if it has one.
fn workbook(cells: &[(&str, f64, Option<&str>)]) -> Vec<u8> {
    let cells: Vec<(&str, &str, String)> = cells
        .iter()
        .map(|&(at, value, formula)| (at, "", format!("{}<v>{value}</v>", formula.unwrap_or(""))))
        .collect();
    let cells: Vec<(&str, &str, &str)> =
        cells.iter().map(|(a, t, x)| (*a, *t, x.as_str())).collect();
    workbook_xml(&cells)
}

/// The same, each cell given as its address, its type (`b` for true or
/// false, or empty for a number) and what it holds as the file has it.
fn workbook_xml(cells: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut rows: Vec<(u32, String)> = Vec::new();
    for &(at, kind, inner) in cells {
        let row: u32 = at
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .parse()
            .unwrap();
        let kind = if kind.is_empty() {
            String::new()
        } else {
            format!(r#" t="{kind}""#)
        };
        let cell = format!(r#"<c r="{at}"{kind}>{inner}</c>"#);
        match rows.last_mut() {
            Some((r, xml)) if *r == row => xml.push_str(&cell),
            _ => rows.push((row, cell)),
        }
    }
    let sheet: String = rows
        .iter()
        .map(|(r, xml)| format!(r#"<row r="{r}">{xml}</row>"#))
        .collect();
    let files = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.to_string(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_string(),
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Model" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_string(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.to_string(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet}</sheetData></worksheet>"#
            ),
        ),
    ];
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, text) in files {
        zip.start_file(name, options).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn model_cell(row: u32, col: u32) -> Cell {
    Cell { sheet: 0, row, col }
}

#[test]
fn a_running_total_two_hundred_thousand_cells_long_is_followed() {
    const LONG: u32 = 200_000;
    let mut cells = vec![("A1".to_string(), 1.0, None)];
    let mut x = 1.0f64;
    for r in 2..=LONG {
        x = x * 1.000_01 + 1.0;
        cells.push((
            format!("A{r}"),
            x,
            Some(format!("<f>A{}*1.00001+1</f>", r - 1)),
        ));
    }
    let cells: Vec<(&str, f64, Option<&str>)> = cells
        .iter()
        .map(|(a, v, f)| (a.as_str(), *v, f.as_deref()))
        .collect();
    let book = Workbook::from_bytes(workbook(&cells)).unwrap();
    let mut model = Model::build(&book, &[model_cell(0, 0)], &[model_cell(LONG - 1, 0)]).unwrap();
    assert_eq!(model.translated(), LONG as usize - 1);
    assert!(model.check().is_empty());
    model.set(&[2.0]).unwrap();
    let mut x = 2.0f64;
    for _ in 1..LONG {
        x = x * 1.000_01 + 1.0;
    }
    assert_eq!(model.value(model_cell(LONG - 1, 0)), Some(x));
}

#[test]
fn shared_formulas_are_read_for_each_cell() {
    // Excel saves a formula filled down once, with the cells below it
    // pointing back to it.
    let book = Workbook::from_bytes(workbook(&[
        ("A1", 3.0, None),
        (
            "A2",
            6.0,
            Some(r#"<f t="shared" ref="A2:A4" si="0">A1*2</f>"#),
        ),
        ("A3", 12.0, Some(r#"<f t="shared" si="0"/>"#)),
        ("A4", 24.0, Some(r#"<f t="shared" si="0"/>"#)),
    ]))
    .unwrap();
    let mut model = Model::build(&book, &[model_cell(0, 0)], &[model_cell(3, 0)]).unwrap();
    assert_eq!(model.translated(), 3);
    assert!(model.check().is_empty());
    model.set(&[1.0]).unwrap();
    assert_eq!(model.value(model_cell(3, 0)), Some(8.0));
}

#[test]
fn a_cell_that_differs_from_what_excel_saved_is_reported() {
    let book = Workbook::from_bytes(workbook(&[
        ("A1", 3.0, None),
        ("A2", 6.0, Some("<f>A1*2</f>")),
        ("A3", 7.0, Some("<f>A2+2</f>")),
    ]))
    .unwrap();
    let mut model = Model::build(&book, &[model_cell(0, 0)], &[model_cell(2, 0)]).unwrap();
    let wrong = model.check();
    assert_eq!(wrong.len(), 1);
    assert_eq!(wrong[0].to_string(), "Model!A3: 8 here, 7 in Excel");
}

#[test]
fn a_formula_that_cannot_be_read_is_refused_with_its_cell() {
    let book = Workbook::from_bytes(workbook(&[
        ("A1", 3.0, None),
        ("A2", 6.0, Some("<f>SUM({1,2})*A1</f>")),
        ("A3", 7.0, Some("<f>A2+1</f>")),
        // Not in the way, so not read at all.
        ("B1", 1.0, Some("<f>Table1[Total]</f>")),
    ]))
    .unwrap();
    let e = Model::build(&book, &[model_cell(0, 0)], &[model_cell(2, 0)])
        .err()
        .unwrap();
    assert!(
        matches!(&e, Error::Cell { cell, .. } if cell == "Model!A2"),
        "{e}"
    );
}

#[test]
fn only_xlsx_files_are_read() {
    assert!(matches!(Workbook::open("model.xls"), Err(Error::Read(_))));
    assert!(matches!(Workbook::open("model.csv"), Err(Error::Read(_))));
    assert!(matches!(
        Workbook::from_bytes(b"not a workbook".to_vec()),
        Err(Error::Read(_))
    ));
}

#[test]
fn true_and_false_in_a_range_are_passed_over_by_sum_and_read_by_and() {
    // As Excel does it; LibreOffice, which keeps TRUE as 1, adds them up.
    let book = Workbook::from_bytes(workbook_xml(&[
        ("A1", "", "<v>2</v>"),
        ("B1", "b", "<v>1</v>"),
        ("C1", "", "<f>SUM(A1:B1)</f><v>2</v>"),
        ("D1", "", "<f>AVERAGE(A1:B1)</f><v>2</v>"),
        ("E1", "", "<f>IF(AND(A1:B1),A1,0)</f><v>2</v>"),
        ("F1", "", "<f>COUNT(A1:B1)+A1</f><v>3</v>"),
    ]))
    .unwrap();
    let outputs: Vec<Cell> = (2..6).map(|col| model_cell(0, col)).collect();
    let mut model = Model::build(&book, &[model_cell(0, 0)], &outputs).unwrap();
    assert!(model.check().is_empty(), "{:?}", model.check());
    model.set(&[5.0]).unwrap();
    let values: Vec<f64> = outputs.iter().map(|&c| model.value(c).unwrap()).collect();
    assert_eq!(values, [5.0, 5.0, 5.0, 6.0]);
    model.set(&[0.0]).unwrap();
    assert_eq!(model.value(model_cell(0, 4)), Some(0.0));
}

#[test]
fn a_loop_only_on_a_branch_never_taken_is_no_trouble() {
    // B1 and C1 read each other, but only when A2 is 1, which it is not;
    // valuation workbooks with a choice to iterate are built this way. C1
    // reaches the input only through B1.
    let book = Workbook::from_bytes(workbook_xml(&[
        ("A1", "", "<v>2</v>"),
        ("A2", "", "<v>0</v>"),
        ("B1", "", "<f>IF(A2=1,C1,A1*3)</f><v>6</v>"),
        ("C1", "", "<f>B1+1</f><v>7</v>"),
    ]))
    .unwrap();
    let mut model = Model::build(&book, &[model_cell(0, 0)], &[model_cell(0, 2)]).unwrap();
    assert!(model.check().is_empty());
    model.set(&[10.0]).unwrap();
    assert_eq!(model.value(model_cell(0, 2)), Some(31.0));

    // Taken, it is refused.
    let book = Workbook::from_bytes(workbook_xml(&[
        ("A1", "", "<v>2</v>"),
        ("A2", "", "<v>1</v>"),
        ("B1", "", "<f>IF(A2=1,C1,A1*3)</f><v>0</v>"),
        ("C1", "", "<f>B1+1</f><v>0</v>"),
    ]))
    .unwrap();
    let e = Model::build(&book, &[model_cell(0, 0)], &[model_cell(0, 2)])
        .err()
        .unwrap();
    assert!(e.to_string().contains("reads itself"), "{e}");
}
