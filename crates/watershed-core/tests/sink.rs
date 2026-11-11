//! Each test here checks one promise about sending changes to a sink.

use std::sync::{Arc, Mutex};

use watershed_core::prelude::*;
use watershed_core::{ChangeSink, NodeId};

type Seen = Arc<Mutex<Vec<(u64, Vec<String>, usize)>>>;

/// Remembers each call: the update, the names of the changed nodes, and how
/// many nodes the graph had.
struct Recorder(Seen);

impl ChangeSink for Recorder {
    fn update(&mut self, g: &Graph, epoch: u64, changed: &[NodeId]) {
        let names = changed.iter().map(|&id| g.name(id).to_string()).collect();
        self.0.lock().unwrap().push((epoch, names, g.len()));
    }
}

fn recorder() -> (Box<dyn ChangeSink>, Seen) {
    let seen = Seen::default();
    (Box::new(Recorder(seen.clone())), seen)
}

#[test]
fn a_sink_starts_with_every_node_then_gets_only_what_changed() -> Result<(), CalcError> {
    let mut g = Graph::new();
    let temperature = g.input("temperature", 20.0)?;
    let is_hot = g.define("is_hot", temperature.gt(30.0))?;
    let _fan = g.define("fan", when(is_hot).then(100.0).otherwise(0.0))?;
    let (sink, seen) = recorder();
    g.attach_sink(sink);

    g.set(temperature, 21.0)?; // is_hot comes out the same
    g.stabilize();
    g.set(temperature, 35.0)?;
    g.stabilize();
    g.stabilize(); // nothing changes

    let seen = seen.lock().unwrap();
    let names: Vec<(u64, Vec<&str>)> = seen
        .iter()
        .map(|(e, n, _)| (*e, n.iter().map(String::as_str).collect()))
        .collect();
    assert_eq!(
        names,
        [
            (0, vec!["temperature", "is_hot", "fan"]),
            (1, vec!["temperature"]),
            (2, vec!["temperature", "is_hot", "fan"]),
            (3, vec![]),
        ]
    );
    Ok(())
}

#[test]
fn a_sink_sees_nodes_made_between_updates_by_the_graph_growing() -> Result<(), CalcError> {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let (sink, seen) = recorder();
    g.attach_sink(sink);
    g.define("late", a * 2.0)?;
    g.stabilize();

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].2, 1);
    assert_eq!(seen[1], (1, vec![], 2));
    Ok(())
}

#[test]
fn a_detached_sink_gets_nothing_more() -> Result<(), CalcError> {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let (sink, seen) = recorder();
    g.attach_sink(sink);
    assert!(g.detach_sink().is_some());
    g.set(a, 2.0)?;
    g.stabilize();
    assert_eq!(seen.lock().unwrap().len(), 1);
    Ok(())
}
