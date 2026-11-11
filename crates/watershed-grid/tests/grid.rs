//! What the grid promises, one test per promise. Read the names.

use watershed_core::Value;
use watershed_grid::{Agg, Kind, Table};

fn text(s: &str) -> Value {
    Value::Str(s.into())
}

/// Orders: region, qty, price, and `line = qty * price * (1 + tax)`.
fn orders() -> Table {
    let mut t = Table::new();
    t.add_field("region", Kind::Text, Agg::None).unwrap();
    t.add_field("qty", Kind::Number, Agg::Sum).unwrap();
    t.add_field("price", Kind::Number, Agg::Max).unwrap();
    t.add_value("tax", 0.0).unwrap();
    t.add_formula("line", "qty * price * (1 + tax)", Agg::Sum)
        .unwrap();
    for (key, region, qty, price) in [
        ("o-1", "EU", 2.0, 10.0),
        ("o-2", "US", 1.0, 5.0),
        ("o-3", "EU", 4.0, 2.5),
        ("o-4", "APAC", 3.0, 7.0),
        ("o-5", "US", 6.0, 1.0),
    ] {
        t.upsert(key, vec![text(region), Value::F64(qty), Value::F64(price)])
            .unwrap();
    }
    t.update();
    t
}

const REGION: usize = 0;
const QTY: usize = 1;
const PRICE: usize = 2;
const LINE: usize = 3;

#[test]
fn rows_show_in_the_order_they_arrived_with_their_calculated_cells() {
    let t = orders();
    assert_eq!(t.view_keys(), ["o-1", "o-2", "o-3", "o-4", "o-5"]);
    assert_eq!(
        t.rows_json(0, 2),
        r#"[{"k":"o-1","c":["EU",2,10,20]},{"k":"o-2","c":["US",1,5,5]}]"#
    );
    assert_eq!(t.totals_json(), "[null,16,10,62]");
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn asking_for_lines_past_the_end_gives_the_ones_there_are() {
    let t = orders();
    assert_eq!(t.rows_json(4, 50), r#"[{"k":"o-5","c":["US",6,1,6]}]"#);
    assert_eq!(t.rows_json(99, 5), "[]");
}

#[test]
fn editing_a_cell_recalculates_only_that_row_and_the_totals() {
    let mut t = orders();
    t.set_cell("o-3", QTY, "10").unwrap();
    let update = t.update();
    assert_eq!(update.report.inputs_changed, 1);
    // o-3's line, and the totals of qty and line.
    assert_eq!(update.report.recalculated, 3);
    assert!(
        !update.rebuilt,
        "nothing is sorted or filtered, so the view stands"
    );
    assert_eq!(t.value("o-3", LINE).unwrap().to_string(), "25");
    assert_eq!(t.totals_json(), "[null,22,10,77]");
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn a_named_value_reaches_every_row_that_uses_it() {
    let mut t = orders();
    t.set_value("tax", 0.5).unwrap();
    let update = t.update();
    // Five lines and their total.
    assert_eq!(update.report.recalculated, 6);
    assert_eq!(t.totals_json(), "[null,16,10,93]");
}

#[test]
fn what_is_typed_into_a_cell_is_read_as_the_columns_kind() {
    let mut t = orders();
    t.set_cell("o-1", QTY, " 1,250 ").unwrap();
    t.update();
    assert_eq!(t.value("o-1", QTY).unwrap().to_string(), "1250");
    assert_eq!(
        t.set_cell("o-1", QTY, "lots").unwrap_err(),
        "\"lots\" is not a number"
    );
    assert_eq!(
        t.set_cell("o-1", LINE, "5").unwrap_err(),
        "line is calculated (qty * price * (1 + tax)), so it can't be set"
    );
    assert_eq!(
        t.set_cell("o-9", QTY, "5").unwrap_err(),
        "there is no row \"o-9\""
    );
}

#[test]
fn sorting_by_a_number_orders_rows_and_ties_keep_arrival_order() {
    let mut t = orders();
    t.set_sort(&[(LINE, true)]).unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-4", "o-1", "o-3", "o-5", "o-2"]);
    t.set_sort(&[(REGION, false), (QTY, true)]).unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-4", "o-3", "o-1", "o-5", "o-2"]);
    t.set_sort(&[]).unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-2", "o-3", "o-4", "o-5"]);
}

#[test]
fn a_sorted_view_follows_an_edit_to_the_sorted_column() {
    let mut t = orders();
    t.set_sort(&[(LINE, true)]).unwrap();
    t.update();
    t.set_cell("o-2", PRICE, "100").unwrap();
    // One row was set, so only that row is moved.
    let update = t.update();
    assert!(update.patched && !update.rebuilt);
    assert_eq!(t.view_keys()[0], "o-2");
    // An edit to a column the sort does not read leaves the view alone.
    t.set_sort(&[(QTY, false)]).unwrap();
    t.update();
    t.set_cell("o-2", PRICE, "3").unwrap();
    assert!(!t.update().rebuilt);
}

#[test]
fn filters_narrow_the_rows_and_the_totals() {
    let mut t = orders();
    t.set_filter(REGION, "u").unwrap(); // contains, ignoring case: EU and US
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-2", "o-3", "o-5"]);
    t.set_filter(REGION, "=eu").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-3"]);
    assert_eq!(t.totals_json(), "[null,6,10,30]");
    t.set_filter(QTY, ">2").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-3"]);
    t.set_filter(REGION, "").unwrap();
    t.set_filter(QTY, "2..4").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-3", "o-4"]);
    t.set_filter(QTY, "<=1").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-2"]);
    t.set_filter(QTY, "6").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-5"]);
    t.set_filter(QTY, "!=6").unwrap();
    t.update();
    assert_eq!(t.filtered_count(), 4);
    assert_eq!(
        t.set_filter(QTY, ">many").unwrap_err(),
        "\"many\" is not a number"
    );
}

#[test]
fn filtered_totals_follow_edits() {
    let mut t = orders();
    t.set_filter(REGION, "=eu").unwrap();
    t.update();
    t.set_cell("o-1", QTY, "3").unwrap();
    t.update();
    assert_eq!(t.totals_json(), "[null,7,10,40]");
    // A row edited out of the filter leaves the view.
    t.set_cell("o-3", REGION, "US").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1"]);
    assert_eq!(t.totals_json(), "[null,3,10,30]");
}

#[test]
fn the_quick_filter_looks_in_keys_and_text() {
    let mut t = orders();
    t.set_quick_filter("apac");
    t.update();
    assert_eq!(t.view_keys(), ["o-4"]);
    t.set_quick_filter("O-5");
    t.update();
    assert_eq!(t.view_keys(), ["o-5"]);
    t.set_quick_filter("");
    t.update();
    assert_eq!(t.filtered_count(), 5);
}

#[test]
fn grouping_gives_a_line_per_group_with_its_totals_and_closed_groups_hide_their_rows() {
    let mut t = orders();
    t.set_group(Some(REGION)).unwrap();
    t.update();
    assert_eq!(t.view_len(), 3);
    assert_eq!(
        t.rows_json(0, 3),
        concat!(
            r#"[{"g":"APAC","n":1,"o":false,"c":[null,3,7,21]},"#,
            r#"{"g":"EU","n":2,"o":false,"c":[null,6,10,30]},"#,
            r#"{"g":"US","n":2,"o":false,"c":[null,7,5,11]}]"#
        )
    );
    t.toggle_group("EU");
    assert_eq!(t.view_len(), 5);
    assert_eq!(t.view_keys(), ["o-1", "o-3"]);
    t.expand_all(true);
    assert_eq!(t.view_len(), 8);
    t.expand_all(false);
    assert_eq!(t.view_len(), 3);
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn a_groups_totals_follow_an_edit_without_working_the_view_out_again() {
    let mut t = orders();
    t.set_group(Some(REGION)).unwrap();
    t.update();
    t.set_cell("o-5", QTY, "10").unwrap();
    let update = t.update();
    assert!(!update.rebuilt);
    assert!(t.rows_json(2, 1).contains(r#""c":[null,11,5,15]"#));
}

#[test]
fn a_row_edited_into_another_group_moves_there() {
    let mut t = orders();
    t.set_group(Some(REGION)).unwrap();
    t.update();
    t.set_cell("o-4", REGION, "EU").unwrap();
    t.update();
    assert_eq!(t.view_groups(), [("EU", 3), ("US", 2)]);
    assert!(t.rows_json(0, 1).contains(r#""c":[null,9,10,51]"#));
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn groups_sort_by_the_sorted_columns_total_or_by_name() {
    let mut t = orders();
    t.set_group(Some(REGION)).unwrap();
    t.set_sort(&[(LINE, true)]).unwrap();
    t.update();
    assert_eq!(t.view_groups(), [("EU", 2), ("APAC", 1), ("US", 2)]);
    t.set_sort(&[(REGION, true)]).unwrap();
    t.update();
    assert_eq!(t.view_groups(), [("US", 2), ("EU", 2), ("APAC", 1)]);
    // Inside a group, rows follow the sort too.
    t.set_sort(&[(QTY, true)]).unwrap();
    t.toggle_group("US");
    t.update();
    assert_eq!(t.view_keys(), ["o-5", "o-2"]);
}

#[test]
fn with_a_filter_on_group_totals_count_only_the_rows_that_pass() {
    let mut t = orders();
    t.set_group(Some(REGION)).unwrap();
    t.set_filter(QTY, ">=2").unwrap();
    t.update();
    assert_eq!(t.view_groups(), [("APAC", 1), ("EU", 2), ("US", 1)]);
    assert!(
        t.rows_json(2, 1)
            .contains(r#""n":1,"o":false,"c":[null,6,1,6]"#)
    );
    assert!(
        t.explain_group("US", LINE)
            .unwrap()
            .starts_with("A filter is on")
    );
}

#[test]
fn only_text_fields_can_be_grouped_by() {
    let mut t = orders();
    assert_eq!(
        t.set_group(Some(QTY)).unwrap_err(),
        "qty: rows are grouped by a text field"
    );
    t.set_group(None).unwrap();
}

#[test]
fn a_formula_column_can_be_added_to_rows_that_are_already_there() {
    let mut t = orders();
    t.set_group(Some(REGION)).unwrap();
    t.update();
    let col = t.add_formula("big", "line >= 20", Agg::Sum).unwrap();
    let share = t.add_formula("double", "line * 2", Agg::Sum).unwrap();
    t.update();
    assert_eq!(t.value("o-1", col).unwrap().to_string(), "true");
    assert_eq!(t.value("o-2", share).unwrap().to_string(), "10");
    // The new number column has a total in the bottom row and in each group.
    assert_eq!(t.totals_json(), "[null,16,10,62,null,124]");
    assert!(t.rows_json(1, 1).contains(r#""c":[null,6,10,30,null,60]"#));
    assert_eq!(
        t.add_formula("oops", "qty * nothing", Agg::None)
            .unwrap_err(),
        "there is no column or value called \"nothing\""
    );
    assert!(t.add_formula("mixed", "qty + region", Agg::None).is_err());
    assert_eq!(
        t.add_formula("qty", "1", Agg::None).unwrap_err(),
        "there is already a column or value called \"qty\""
    );
    assert_eq!(t.columns(), 6, "a refused column is not added");
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn removed_rows_leave_the_view_and_the_totals_and_their_keys_can_come_back() {
    let mut t = orders();
    assert!(t.remove("o-2").unwrap());
    assert!(!t.remove("o-2").unwrap());
    t.remove("o-4").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-3", "o-5"]);
    assert_eq!(t.row_count(), 3);
    assert_eq!(t.totals_json(), "[null,12,10,36]");
    t.upsert("o-2", vec![text("EU"), Value::F64(1.0), Value::F64(1.0)])
        .unwrap();
    t.set_cell("o-5", QTY, "7").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-3", "o-5", "o-2"]);
    assert_eq!(t.value("o-5", LINE).unwrap().to_string(), "7");
    assert_eq!(t.totals_json(), "[null,14,10,38]");
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn a_cell_can_say_why_it_has_its_value() {
    let t = orders();
    let why = t.explain("o-1", LINE).unwrap();
    assert!(why.starts_with("row[0].line = 20"), "{why}");
    assert!(why.contains("row[0].qty = 2  (input)"), "{why}");
    assert!(why.contains("tax = 0  (input)"), "{why}");
    let total = t.explain_total(LINE).unwrap();
    assert!(total.starts_with("sum(line) = 62"), "{total}");
    assert_eq!(t.explain_total(REGION).unwrap_err(), "region has no total");
}

#[test]
fn the_view_exports_as_csv() {
    let mut t = orders();
    t.set_filter(REGION, "=us").unwrap();
    t.set_sort(&[(QTY, true)]).unwrap();
    t.upsert(
        "o-6",
        vec![text("US, \"west\""), Value::F64(9.0), Value::F64(1.0)],
    )
    .unwrap();
    t.set_filter(REGION, "us").unwrap();
    t.update();
    assert_eq!(
        t.csv(),
        "key,region,qty,price,line\no-6,\"US, \"\"west\"\"\",9,1,9\no-5,US,6,1,6\no-2,US,1,5,5\n"
    );
}

/// A small random number generator, so the test below is the same every run.
fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn through_random_edits_the_grids_totals_match_adding_the_rows_up() {
    let regions = ["EU", "US", "APAC", "LATAM"];
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut t = Table::new();
    t.add_field("region", Kind::Text, Agg::None).unwrap();
    t.add_field("qty", Kind::Number, Agg::Sum).unwrap();
    t.add_formula("double", "qty * 2", Agg::Sum).unwrap();
    t.set_group(Some(0)).unwrap();
    let mut model: Vec<(String, &str, f64)> = Vec::new();
    let mut made = 0;
    for step in 0..400 {
        match next(&mut seed) % 5 {
            0 | 1 => {
                made += 1;
                let (region, qty) = (
                    regions[(next(&mut seed) % 4) as usize],
                    (next(&mut seed) % 50) as f64,
                );
                let key = format!("k{made}");
                t.upsert(&key, vec![text(region), Value::F64(qty)]).unwrap();
                model.push((key, region, qty));
            }
            2 if !model.is_empty() => {
                let k = (next(&mut seed) % model.len() as u64) as usize;
                let qty = (next(&mut seed) % 50) as f64;
                t.set_cell(&model[k].0, 1, &qty.to_string()).unwrap();
                model[k].2 = qty;
            }
            3 if !model.is_empty() => {
                let k = (next(&mut seed) % model.len() as u64) as usize;
                let region = regions[(next(&mut seed) % 4) as usize];
                t.set_cell(&model[k].0, 0, region).unwrap();
                model[k].1 = region;
            }
            4 if !model.is_empty() => {
                let k = (next(&mut seed) % model.len() as u64) as usize;
                assert!(t.remove(&model.remove(k).0).unwrap());
            }
            _ => {}
        }
        if step % 3 == 0 {
            // Sometimes several changes land in one update.
            continue;
        }
        t.update();
        // `+ 0.0`: the sum of no rows is -0, which the grid shows as 0.
        let total = model.iter().map(|m| m.2).sum::<f64>() + 0.0;
        assert_eq!(
            t.totals_json(),
            format!("[null,{total},{}]", total * 2.0),
            "step {step}"
        );
        let mut expected: Vec<(&str, usize)> = regions
            .iter()
            .map(|&r| (r, model.iter().filter(|m| m.1 == r).count()))
            .filter(|&(_, n)| n > 0)
            .collect();
        expected.sort();
        assert_eq!(t.view_groups(), expected, "step {step}");
        let lines = t.rows_json(0, 10);
        for (region, rows) in expected {
            let sum: f64 = model.iter().filter(|m| m.1 == region).map(|m| m.2).sum();
            let line = format!(
                r#"{{"g":"{region}","n":{rows},"o":false,"c":[null,{sum},{}]}}"#,
                sum * 2.0
            );
            assert!(lines.contains(&line), "step {step}: no {line} in {lines}");
        }
        assert_eq!(t.row_count(), model.len());
        assert_eq!(t.check(), Ok(()), "step {step}");
    }
}

/// Scores: team (counted), and points with an average.
fn scores() -> Table {
    let mut t = Table::new();
    t.add_field("team", Kind::Text, Agg::Count).unwrap();
    t.add_field("points", Kind::Number, Agg::Avg).unwrap();
    for (key, team, points) in [
        ("a", "red", 10.0),
        ("b", "red", 20.0),
        ("c", "blue", 3.0),
        ("d", "red", 30.0),
    ] {
        t.upsert(key, vec![text(team), Value::F64(points)]).unwrap();
    }
    t.update();
    t
}

#[test]
fn averages_and_counts_show_in_the_bottom_row_and_in_each_group() {
    let mut t = scores();
    assert_eq!(t.totals_json(), "[4,15.75]");
    t.set_group(Some(0)).unwrap();
    t.update();
    assert_eq!(
        t.groups_json(),
        r#"[{"g":"blue","n":1,"c":[1,3]},{"g":"red","n":3,"c":[3,20]}]"#
    );
    // An edit moves the averages without the view being worked out again.
    t.set_cell("d", 1, "60").unwrap();
    assert!(!t.update().rebuilt);
    assert_eq!(t.totals_json(), "[4,23.25]");
    assert!(t.groups_json().contains(r#""g":"red","n":3,"c":[3,30]"#));
    // With a filter on, they are of the rows that pass.
    t.set_filter(1, ">=20").unwrap();
    t.update();
    assert_eq!(t.totals_json(), "[2,40]");
    assert_eq!(t.groups_json(), r#"[{"g":"red","n":2,"c":[2,40]}]"#);
    assert!(
        t.explain_total(1)
            .unwrap()
            .contains("The average is this sum divided by the number of rows.")
    );
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn groups_can_be_sorted_by_their_average() {
    let mut t = scores();
    t.set_group(Some(0)).unwrap();
    t.set_sort(&[(1, true)]).unwrap();
    t.update();
    assert_eq!(t.view_groups(), [("red", 3), ("blue", 1)]);
}

#[test]
fn an_average_of_no_rows_shows_nothing() {
    let mut t = scores();
    t.set_filter(1, ">1000").unwrap();
    t.update();
    assert_eq!(t.totals_json(), "[0,null]");
}

#[test]
fn only_number_columns_have_totals_but_any_column_can_be_counted() {
    let mut t = Table::new();
    assert!(t.add_field("name", Kind::Text, Agg::Sum).is_err());
    t.add_field("name", Kind::Text, Agg::Count).unwrap();
}

#[test]
fn a_groups_line_can_be_found_in_the_view() {
    let mut t = scores();
    assert_eq!(t.groups_json(), "[]");
    t.set_group(Some(0)).unwrap();
    t.update();
    t.toggle_group("blue");
    assert!(t.group_is_open("blue"));
    assert_eq!(t.group_line("blue"), Some(0));
    assert_eq!(t.group_line("red"), Some(2));
    assert_eq!(t.group_line("green"), None);
}

#[test]
fn with_history_kept_a_cell_and_a_total_can_say_how_they_moved() {
    let mut t = Table::new();
    t.keep_history(100);
    t.add_field("team", Kind::Text, Agg::None).unwrap();
    t.add_field("points", Kind::Number, Agg::Sum).unwrap();
    t.upsert("a", vec![text("red"), Value::F64(1.0)]).unwrap();
    t.upsert("b", vec![text("red"), Value::F64(10.0)]).unwrap();
    t.update();
    for points in ["2", "3", "5"] {
        t.set_cell("a", 1, points).unwrap();
        t.update();
    }
    // Line 0 is `a`, which moved; line 1 is `b`, which did not.
    assert_eq!(t.trends_json(0, 2, 1, 16), "[[1,2,3,5],[10,10,10,10]]");
    assert_eq!(t.total_trend_json(1, 16), "[11,12,13,15]");
    // Fewer points than updates: evenly spaced, ending at the latest.
    assert_eq!(t.trends_json(0, 1, 1, 2), "[[1,5]]");
    // A text column has no numbers to show.
    assert_eq!(t.trends_json(0, 1, 0, 16), "[null]");
    assert_eq!(t.total_trend_json(0, 16), "null");
    // A group's line gives its total's history.
    t.set_group(Some(0)).unwrap();
    t.update();
    t.set_cell("b", 1, "20").unwrap();
    t.update();
    // The group's total was made when rows were grouped, so it starts there.
    assert_eq!(t.trends_json(0, 1, 1, 64), "[[15,25]]");
}

#[test]
fn without_history_there_are_no_trends() {
    let t = scores();
    assert_eq!(t.trends_json(0, 2, 1, 16), "[null,null]");
    assert_eq!(t.total_trend_json(1, 16), "null");
}

#[test]
fn a_text_filter_can_name_several_values() {
    let mut t = orders();
    t.set_filter(REGION, "=eu|APAC").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["o-1", "o-3", "o-4"]);
    assert_eq!(
        t.values_json(REGION, 10),
        r#"{"more":false,"values":[["APAC",1],["EU",2],["US",2]]}"#
    );
    assert_eq!(t.values_json(REGION, 2), r#"{"more":true,"values":[]}"#);
}

#[test]
fn an_update_with_nothing_to_apply_does_not_use_up_history() {
    let mut t = orders();
    let before = t.update().report.epoch;
    t.set_sort(&[(QTY, true)]).unwrap();
    let after = t.update();
    assert!(after.rebuilt);
    assert_eq!(after.report.epoch, before);
}

#[test]
fn the_grid_can_show_itself_as_of_an_earlier_update() {
    let mut t = Table::new();
    t.keep_history(100);
    t.add_field("team", Kind::Text, Agg::None).unwrap();
    t.add_field("points", Kind::Number, Agg::Sum).unwrap();
    t.upsert("a", vec![text("red"), Value::F64(1.0)]).unwrap();
    t.upsert("b", vec![text("blue"), Value::F64(10.0)]).unwrap();
    let first = t.update().report.epoch;
    t.set_cell("a", 1, "50").unwrap();
    t.upsert("c", vec![text("red"), Value::F64(7.0)]).unwrap();
    let second = t.update().report.epoch;
    t.set_sort(&[(1, true)]).unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["a", "b", "c"]);
    assert_eq!(t.totals_json(), "[null,67]");

    // Back then: `a` had 1 point, so `b` led, and `c` was not there.
    t.set_as_of(Some(first)).unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["b", "a"]);
    assert_eq!(t.totals_json(), "[null,11]");
    assert_eq!(
        t.rows_json(0, 2),
        r#"[{"k":"b","c":["blue",10]},{"k":"a","c":["red",1]}]"#
    );
    // A filter looks at the values of then, and its totals add those up.
    t.set_filter(1, "<5").unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["a"]);
    assert_eq!(t.totals_json(), "[null,1]");
    t.set_filter(1, "").unwrap();

    // Nothing can be changed while looking back.
    assert_eq!(
        t.set_cell("a", 1, "9").unwrap_err(),
        format!("the grid is showing update {first}; go back to now to change it")
    );
    assert!(t.remove("a").is_err());
    assert!(
        t.set_as_of(Some(second + 5))
            .unwrap_err()
            .contains("is not kept")
    );

    t.set_as_of(None).unwrap();
    t.update();
    assert_eq!(t.view_keys(), ["a", "b", "c"]);
    assert_eq!(t.totals_json(), "[null,67]");
    t.set_cell("a", 1, "9").unwrap();
    t.update();
    assert_eq!(t.totals_json(), "[null,26]");
    assert_eq!(t.check(), Ok(()));
}

#[test]
fn looking_back_needs_history() {
    let mut t = orders();
    assert_eq!(
        t.set_as_of(Some(1)).unwrap_err(),
        "the grid is not keeping earlier values"
    );
}

/// Everything the view shows, as one text to compare.
fn shown(t: &Table) -> String {
    format!(
        "{:?}\n{}\n{}\n{}",
        t.view_keys(),
        t.rows_json(0, 10_000),
        t.groups_json(),
        t.totals_json()
    )
}

#[test]
fn a_view_kept_up_to_date_row_by_row_is_the_one_a_fresh_start_gives() {
    let regions = ["EU", "US", "APAC", "LATAM"];
    let sides = ["Buy", "Sell"];
    let mut seed = 0x1234_5678_9ABC_DEF1_u64;
    let mut pick = |n: u64| (next(&mut seed) % n) as usize;
    let (mut patched, mut rebuilt) = (0, 0);
    // Every mix of grouping, sorting and filtering, each through its own edits.
    for mix in 0..48 {
        let mut t = Table::new();
        t.add_field("region", Kind::Text, Agg::Count).unwrap();
        t.add_field("side", Kind::Text, Agg::None).unwrap();
        t.add_field("qty", Kind::Number, Agg::Sum).unwrap();
        t.add_field("price", Kind::Number, Agg::Avg).unwrap();
        t.add_formula("line", "qty * price", Agg::Sum).unwrap();
        t.add_formula("signed", "if(side == \"Buy\", 1, -1) * line", Agg::Max)
            .unwrap();
        let rows = 600;
        for r in 0..rows {
            t.upsert(
                &format!("k{r:03}"),
                vec![
                    text(regions[pick(4)]),
                    text(sides[pick(2)]),
                    // Few distinct values, so many rows tie.
                    Value::F64(pick(6) as f64),
                    Value::F64(pick(40) as f64),
                ],
            )
            .unwrap();
        }
        t.set_group([None, Some(0), Some(1)][mix % 3]).unwrap();
        let sorts: [&[(usize, bool)]; 4] = [
            &[],
            &[(4, true)],
            &[(2, false), (5, true)],
            &[(0, true), (3, false)],
        ];
        t.set_sort(sorts[(mix / 3) % 4]).unwrap();
        let filters = [(2, ""), (2, ">=2"), (5, "<0"), (1, "=buy")];
        let (col, filter) = filters[(mix / 12) % 4];
        t.set_filter(col, filter).unwrap();
        t.expand_all(true);
        t.update();
        t.expand_all(true);

        for step in 0..40 {
            // A few rows change at once: numbers, and now and then text.
            for _ in 0..1 + pick(5) {
                let key = format!("k{:03}", pick(rows));
                match pick(10) {
                    0 => t.set_cell(&key, 0, regions[pick(4)]).unwrap(),
                    1 => t.set_cell(&key, 1, sides[pick(2)]).unwrap(),
                    2..=5 => t.set_cell(&key, 2, &pick(6).to_string()).unwrap(),
                    _ => t.set_cell(&key, 3, &pick(40).to_string()).unwrap(),
                }
            }
            let update = t.update();
            patched += usize::from(update.patched);
            rebuilt += usize::from(update.rebuilt);
            let kept = shown(&t);
            t.rebuild_view();
            assert_eq!(kept, shown(&t), "mix {mix}, step {step}");
        }
        assert_eq!(t.check(), Ok(()));
    }
    // Both ways of keeping the view were exercised.
    assert!(patched > 500, "only {patched} updates moved rows");
    assert!(rebuilt > 20, "only {rebuilt} updates started again");
}

#[test]
fn when_many_rows_change_at_once_the_view_is_worked_out_from_scratch() {
    let mut t = Table::new();
    t.add_field("n", Kind::Number, Agg::Sum).unwrap();
    for r in 0..1000 {
        t.upsert(&format!("k{r}"), vec![Value::F64(r as f64)])
            .unwrap();
    }
    t.set_sort(&[(0, true)]).unwrap();
    t.update();
    for r in 0..500 {
        t.set_cell(&format!("k{r}"), 0, &(5000 - r).to_string())
            .unwrap();
    }
    let update = t.update();
    assert!(update.rebuilt && !update.patched);
    assert_eq!(t.view_keys()[0], "k0");
    // A named value reaches every row, so that starts again too.
    let mut t = orders();
    t.set_sort(&[(LINE, true)]).unwrap();
    t.update();
    t.set_value("tax", 1.0).unwrap();
    assert!(t.update().rebuilt);
}
