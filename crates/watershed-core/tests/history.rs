//! Each test here checks one promise about reading values as of an earlier
//! update. The random counterpart is in `properties.rs`.

use watershed_core::Value;
use watershed_core::prelude::*;

type Result = std::result::Result<(), CalcError>;

#[test]
fn a_node_reads_as_it_was_after_any_earlier_update() -> Result {
    let mut g = Graph::new();
    let price = g.input("price", 1.0)?;
    let total = g.define("total", price * 10.0)?;
    g.keep_history(u64::MAX);

    for p in [2.0, 3.0, 4.0] {
        g.set(price, p)?;
        g.stabilize();
    }

    for (epoch, p) in [(0, 1.0), (1, 2.0), (2, 3.0), (3, 4.0)] {
        assert_eq!(g.get_at(price, epoch)?, p);
        assert_eq!(g.get_at(total, epoch)?, p * 10.0);
    }
    Ok(())
}

#[test]
fn a_change_waiting_for_the_next_update_is_not_history() -> Result {
    let mut g = Graph::new();
    let price = g.input("price", 1.0)?;
    g.keep_history(u64::MAX);
    g.set(price, 2.0)?;
    assert_eq!(g.get_at(price, 0)?, 1.0);
    Ok(())
}

#[test]
fn only_values_that_change_are_recorded() -> Result {
    let mut g = Graph::new();
    let temperature = g.input("temperature", 20.0)?;
    let other = g.input("other", 1.0)?;
    let is_hot = g.define("is_hot", temperature.gt(30.0))?;
    let unrelated = g.define("unrelated", other * 2.0)?;
    g.keep_history(u64::MAX);

    // Only the input changes: is_hot comes out the same.
    g.set(temperature, 21.0)?;
    g.stabilize();
    assert_eq!(g.history_versions(), 1);

    // Setting an input to the value it has records nothing.
    g.set(temperature, 21.0)?;
    g.stabilize();
    assert_eq!(g.history_versions(), 1);

    assert!(!g.get_at(is_hot, 0)?);
    assert_eq!(g.get_at(temperature, 0)?, 20.0);
    assert_eq!(g.get_at(unrelated, 0)?, 2.0);
    Ok(())
}

#[test]
fn a_node_does_not_exist_before_the_update_after_it_was_made() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.keep_history(u64::MAX);
    g.stabilize();

    let late = g.define("late", a * 2.0)?;
    assert!(matches!(
        g.get_at(late, 1),
        Err(CalcError::NotCreatedYet { epoch: 1, .. })
    ));

    g.stabilize();
    assert_eq!(g.get_at(late, 2)?, 2.0);
    assert!(matches!(
        g.get_at(late, 1),
        Err(CalcError::NotCreatedYet { .. })
    ));
    Ok(())
}

#[test]
fn a_node_made_between_updates_keeps_its_first_value_in_history() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.keep_history(u64::MAX);
    g.stabilize();
    let late = g.define("late", a * 2.0)?;
    g.stabilize();

    g.set(a, 5.0)?;
    g.stabilize();

    assert_eq!(g.get_at(late, 2)?, 2.0);
    assert_eq!(g.get_at(late, 3)?, 10.0);
    Ok(())
}

#[test]
fn updates_before_history_started_cannot_be_read() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.set(a, 2.0)?;
    g.stabilize();
    g.keep_history(u64::MAX);
    g.set(a, 3.0)?;
    g.stabilize();

    assert_eq!(g.history_range(), Some((1, 2)));
    assert_eq!(g.get_at(a, 1)?, 2.0);
    assert_eq!(
        g.get_at(a, 0),
        Err(CalcError::EpochUnavailable {
            epoch: 0,
            oldest: 1,
            latest: 2
        })
    );
    Ok(())
}

#[test]
fn an_update_that_has_not_happened_cannot_be_read() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.keep_history(u64::MAX);
    assert!(matches!(
        g.get_at(a, 1),
        Err(CalcError::EpochUnavailable { .. })
    ));
    Ok(())
}

#[test]
fn reading_history_while_it_is_off_says_so() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    assert_eq!(g.get_at(a, 0), Err(CalcError::HistoryOff));
    g.keep_history(1);
    g.stop_history();
    assert_eq!(g.get_at(a, 0), Err(CalcError::HistoryOff));
    assert_eq!(g.history_range(), None);
    Ok(())
}

#[test]
fn only_the_last_updates_asked_for_are_kept() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 0.0)?;
    g.keep_history(2);

    for i in 1..=10 {
        g.set(a, f64::from(i))?;
        g.stabilize();
    }

    assert_eq!(g.history_range(), Some((8, 10)));
    assert_eq!(g.history_versions(), 2);
    assert_eq!(g.get_at(a, 8)?, 8.0);
    assert_eq!(g.get_at(a, 9)?, 9.0);
    assert_eq!(g.get_at(a, 10)?, 10.0);
    assert!(matches!(
        g.get_at(a, 7),
        Err(CalcError::EpochUnavailable { .. })
    ));
    Ok(())
}

#[test]
fn a_long_unchanged_value_stays_readable_after_older_updates_are_forgotten() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let b = g.input("b", 0.0)?;
    g.keep_history(3);

    // `a` changes once, then `b` keeps changing, so `a`'s old value came from
    // long before the oldest readable update.
    g.set(a, 2.0)?;
    g.stabilize();
    for i in 1..=20 {
        g.set(b, f64::from(i))?;
        g.stabilize();
    }
    g.set(a, 3.0)?;
    g.stabilize();

    assert_eq!(g.history_range(), Some((19, 22)));
    assert_eq!(g.get_at(a, 19)?, 2.0);
    assert_eq!(g.get_at(a, 22)?, 3.0);
    Ok(())
}

#[test]
fn shortening_history_forgets_straight_away_and_lengthening_brings_nothing_back() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 0.0)?;
    g.keep_history(u64::MAX);
    for i in 1..=5 {
        g.set(a, f64::from(i))?;
        g.stabilize();
    }

    g.keep_history(1);
    assert_eq!(g.history_range(), Some((4, 5)));
    assert_eq!(g.history_versions(), 1);

    g.keep_history(u64::MAX);
    assert_eq!(g.history_range(), Some((4, 5)));
    Ok(())
}

#[test]
fn a_redefined_node_keeps_the_values_of_its_old_formula() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 3.0)?;
    let n = g.define("n", a * 2.0)?;
    g.keep_history(u64::MAX);

    g.redefine(n, a * 100.0)?;
    assert_eq!(g.get_at(n, 0)?, 6.0);
    g.stabilize();

    assert_eq!(g.get_at(n, 0)?, 6.0);
    assert_eq!(g.get_at(n, 1)?, 300.0);
    Ok(())
}

#[test]
fn an_error_in_the_past_is_reported_as_an_error() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1_i64)?;
    let b = g.input("b", 0_i64)?;
    let ratio = g.define("ratio", a / b)?;
    g.keep_history(u64::MAX);
    g.set(b, 1)?;
    g.stabilize();

    assert!(matches!(
        g.get_at(ratio, 0),
        Err(CalcError::Evaluation { .. })
    ));
    assert!(matches!(g.value_at(ratio.id(), 0)?, Value::Error(_)));
    assert_eq!(g.get_at(ratio, 1)?, 1);
    Ok(())
}

#[test]
fn a_collection_total_reads_as_it_was_before_rows_changed() -> Result {
    let mut g = Graph::new();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty")?;
    let total = orders.sum(&mut g, "total", qty)?;
    g.keep_history(u64::MAX);

    orders.upsert(&mut g, "a", &[qty.is(1.0)])?;
    g.stabilize();
    orders.upsert(&mut g, "b", &[qty.is(2.0)])?;
    g.stabilize();
    orders.remove(&mut g, "a")?;
    g.stabilize();
    // Reuses the removed row's nodes.
    orders.upsert(&mut g, "c", &[qty.is(10.0)])?;
    g.stabilize();

    let at = |g: &Graph, e| g.get_at(total, e).unwrap();
    assert_eq!(
        (1..=4).map(|e| at(&g, e)).collect::<Vec<f64>>(),
        [1.0, 3.0, 2.0, 12.0]
    );
    Ok(())
}

#[test]
fn a_row_reads_as_it_was_before_it_changed_or_was_removed() -> Result {
    let mut g = Graph::new();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty")?;
    let doubled = orders.column(&mut g, "doubled", qty * 2.0)?;
    g.keep_history(u64::MAX);

    orders.upsert(&mut g, "a", &[qty.is(1.0)])?;
    g.stabilize();
    orders.upsert(&mut g, "a", &[qty.is(5.0)])?;
    g.stabilize();
    orders.remove(&mut g, "a")?;
    g.stabilize();

    assert!(!orders.contains_at(&g, "a", 0)?);
    assert_eq!(orders.get_at(&g, "a", qty, 1)?, 1.0);
    assert_eq!(orders.get_at(&g, "a", doubled, 2)?, 10.0);
    assert!(!orders.contains_at(&g, "a", 3)?);
    assert!(matches!(
        orders.get_at(&g, "a", qty, 3),
        Err(CalcError::Invalid(_))
    ));
    assert_eq!(orders.keys_at(&g, 2)?, ["a"].map(String::from).to_vec());
    Ok(())
}

#[test]
fn a_reused_row_belongs_to_its_new_key_from_the_update_that_reused_it() -> Result {
    let mut g = Graph::new();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty")?;
    g.keep_history(u64::MAX);

    orders.upsert(&mut g, "old", &[qty.is(1.0)])?;
    g.stabilize();
    orders.remove(&mut g, "old")?;
    orders.upsert(&mut g, "new", &[qty.is(2.0)])?; // takes the same row's nodes
    g.stabilize();
    orders.upsert(&mut g, "old", &[qty.is(3.0)])?; // back, in a row of its own
    g.stabilize();

    assert_eq!(orders.keys_at(&g, 1)?, ["old"].map(String::from).to_vec());
    assert_eq!(orders.keys_at(&g, 2)?, ["new"].map(String::from).to_vec());
    assert_eq!(orders.get_at(&g, "old", qty, 1)?, 1.0);
    assert_eq!(orders.get_at(&g, "new", qty, 2)?, 2.0);
    assert_eq!(orders.get_at(&g, "old", qty, 3)?, 3.0);
    assert!(!orders.contains_at(&g, "old", 2)?);
    Ok(())
}

#[test]
fn rows_changed_just_before_history_starts_read_as_they_were() -> Result {
    let mut g = Graph::new();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty")?;
    orders.upsert(&mut g, "old", &[qty.is(1.0)])?;
    g.stabilize();

    // Made while history is off, counting from the next update.
    orders.remove(&mut g, "old")?;
    orders.upsert(&mut g, "new", &[qty.is(2.0)])?; // the same row's nodes
    g.keep_history(u64::MAX);
    g.stabilize();

    assert_eq!(orders.keys_at(&g, 1)?, ["old"].map(String::from).to_vec());
    assert_eq!(orders.get_at(&g, "old", qty, 1)?, 1.0);
    assert_eq!(orders.keys_at(&g, 2)?, ["new"].map(String::from).to_vec());
    Ok(())
}
