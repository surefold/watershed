//! Whether a plan holds together: every number in it that points somewhere
//! points inside what it points into, and every node's instructions leave
//! one value on a stack no deeper than [`MAX_STACK`]. A plan made by
//! [`Scenarios`] always does; one read from a file is checked, so that a
//! file that was not written by `save` is refused rather than run, where it
//! could index out of bounds on the CPU or read memory it should not on a
//! GPU.

use super::{
    CODE, CODE_BITS, CONST, COS, LOAD, LOAD_SHARED, MAX_ARG, MAX_STACK, Output, POW, Scenarios,
    arity,
};
use crate::fuse::LOCAL;

/// Whether `code` is an instruction a node's program can hold (`LOCAL`
/// only in a cluster's shape).
fn known(code: u32) -> bool {
    code <= POW || (code > LOCAL && code <= COS)
}

/// Checks one node's instructions, given what each kind of load may read:
/// the stack never runs dry or deeper than `deepest`, and ends with one
/// value. `load` checks a load's code and number.
fn program(
    ops: &[u32],
    deepest: usize,
    load: &mut dyn FnMut(u32, u32) -> Result<(), String>,
) -> Result<(), String> {
    let mut depth = 0usize;
    for &op in ops {
        let code = op & CODE;
        if !known(code) {
            return Err(format!("an instruction has the unknown code {code}"));
        }
        let takes = arity(code);
        if takes == 0 {
            load(code, op >> CODE_BITS)?;
        } else if depth < takes {
            return Err("an instruction takes more than the stack holds".into());
        }
        depth = depth + 1 - takes;
        if depth > deepest {
            return Err(format!("a program needs a stack deeper than {deepest}"));
        }
    }
    match depth {
        1 => Ok(()),
        _ => Err("a program does not leave one value".into()),
    }
}

fn within(what: &str, at: usize, len: usize) -> Result<(), String> {
    match at < len {
        true => Ok(()),
        false => Err(format!("{what} {at} is past the {len} there are")),
    }
}

impl Scenarios {
    /// Whether the plan holds together (see the module's documentation).
    pub(crate) fn check(&self) -> Result<(), String> {
        let (slots, consts, shared) = (self.slots, self.consts.len(), self.shared.len());
        if self.vary == 0 || self.vary > slots {
            return Err("the varied inputs do not fit in the slots".into());
        }
        if self.programs.len() % 3 != 0 {
            return Err("the programs are not in threes".into());
        }
        let nodes = self.programs.len() / 3;
        // A slot is numbered in what an instruction holds beside its code,
        // and there is at most one for each input and each node: a file that
        // asks for more would have a scenario take memory it never uses.
        let most = self.vary.saturating_add(nodes).min(MAX_ARG + 1);
        if self.vary > MAX_ARG || slots > most {
            return Err(format!("{slots} slots a scenario, for {nodes} nodes"));
        }
        if self.fused.as_ref().is_some_and(|fused| fused.slots > most) {
            return Err("too many slots a scenario for the clusters".into());
        }
        let reads = |code: u32, at: u32, shared_now: usize| -> Result<(), String> {
            match code {
                LOAD => within("a slot", at as usize, slots),
                CONST => within("a constant", at as usize, consts),
                LOAD_SHARED => within("a shared value", at as usize, shared_now),
                _ => Err(format!("an instruction with the code {code} reads nothing")),
            }
        };

        // Every node's instructions, and where its value goes.
        if self.operand_at.len() != nodes {
            return Err("not every node says where its operands are".into());
        }
        for (n, program_of) in self.programs.chunks_exact(3).enumerate() {
            let (start, len, slot) = (
                program_of[0] as usize,
                program_of[1] as usize,
                program_of[2],
            );
            let end = start.checked_add(len).ok_or("a program is too long")?;
            if end > self.ops.len() {
                return Err(format!("node {n}'s program is past the instructions"));
            }
            // A node writes a slot of its own, never a varied input's.
            if (slot as usize) < self.vary || slot as usize >= slots {
                return Err(format!("node {n} writes slot {slot}"));
            }
            let ops = &self.ops[start..end];
            program(ops, MAX_STACK, &mut |code, at| reads(code, at, shared))?;
            // Its operands are what its loads read, in order.
            let at = self.operand_at[n] as usize;
            let loads: Vec<u32> = ops
                .iter()
                .filter(|op| arity(*op & CODE) == 0)
                .map(|op| op >> CODE_BITS)
                .collect();
            if self.operands.get(at..at + loads.len()) != Some(&loads[..]) {
                return Err(format!("node {n}'s operands are not what it reads"));
            }
        }

        // The levels cover the nodes in order.
        let mut next = 0;
        for level in &self.levels {
            if level.start as usize != next || level.end < level.start {
                return Err("the levels do not follow one another".into());
            }
            next = level.end as usize;
        }
        if next != nodes {
            return Err("the levels do not cover the nodes".into());
        }

        // Each group is a run of nodes of its shape.
        let mut next = 0;
        for group in &self.groups {
            let shape = self
                .shapes
                .get(group.shape as usize)
                .ok_or("a group's shape is not one of the plan's")?;
            if group.first as usize != next {
                return Err("the groups do not follow one another".into());
            }
            next = (group.first as usize)
                .checked_add(group.count as usize)
                .filter(|&end| end <= nodes)
                .ok_or("a group is past the nodes")?;
            for n in group.first as usize..next {
                let (start, len) = (
                    self.programs[3 * n] as usize,
                    self.programs[3 * n + 1] as usize,
                );
                let codes = self.ops[start..start + len].iter().map(|op| op & CODE);
                if !codes.eq(shape.iter().copied()) {
                    return Err(format!("node {n} is not of its group's shape"));
                }
            }
        }
        if next != nodes {
            return Err("the groups do not cover the nodes".into());
        }

        // The shared values: the inputs, then each node worked out once,
        // reading only the shared values before it.
        if self.shared_programs.len() % 2 != 0
            || self.shared_inputs + self.shared_programs.len() / 2 != shared
        {
            return Err("the shared values do not match their programs".into());
        }
        for (k, pair) in self.shared_programs.chunks_exact(2).enumerate() {
            let (start, len) = (pair[0] as usize, pair[1] as usize);
            let ops = start
                .checked_add(len)
                .and_then(|end| self.shared_ops.get(start..end))
                .ok_or("a shared program is past its instructions")?;
            let before = self.shared_inputs + k;
            program(ops, MAX_STACK, &mut |code, at| match code {
                LOAD => Err("a shared program reads a slot".into()),
                _ => reads(code, at, before),
            })?;
        }

        let output = |output: &Output, slots: usize| match *output {
            Output::Slot(slot) => within("an output's slot", slot as usize, slots),
            Output::Shared(at) => within("an output's shared value", at as usize, shared),
            Output::Fixed(_) => Ok(()),
        };
        for out in &self.outputs {
            output(out, slots)?;
        }

        // The clusters, if any.
        // Without clusters, their outputs are the plain ones.
        let Some(fused) = &self.fused else {
            for out in &self.fused_outputs {
                output(out, slots)?;
            }
            return Ok(());
        };
        let fused_slots = fused.slots;
        if fused_slots < self.vary {
            return Err("the varied inputs do not fit in the clusters' slots".into());
        }
        if fused.slot_of.len() != nodes {
            return Err("not every node says where clusters write it".into());
        }
        for slot in fused.slot_of.iter().flatten() {
            within("a cluster's slot", *slot as usize, fused_slots)?;
        }
        for out in &self.fused_outputs {
            output(out, fused_slots)?;
        }
        // Each shape: its nodes, each how many instructions, whether it goes
        // to memory, then the instructions, which can read the shape's
        // earlier nodes. What it reads from outside, and how many it writes.
        let mut needs: Vec<(usize, Vec<u32>)> = Vec::with_capacity(fused.shapes.len());
        for shape in &fused.shapes {
            let (mut at, mut node, mut writes, mut loads) = (0, 0u32, 0, Vec::new());
            while at < shape.len() {
                let len = *shape.get(at).ok_or("a cluster's shape is cut short")? as usize;
                let memory = *shape.get(at + 1).ok_or("a cluster's shape is cut short")?;
                if memory > 1 {
                    return Err("a cluster's node neither goes to memory nor stays".into());
                }
                writes += memory as usize;
                let ops = (at + 2)
                    .checked_add(len)
                    .and_then(|end| shape.get(at + 2..end))
                    .ok_or("a cluster's shape is cut short")?;
                let mut depth = 0usize;
                for &op in ops {
                    let code = op & CODE;
                    if code == LOCAL {
                        within(
                            "a cluster's node",
                            (op >> CODE_BITS) as usize,
                            node as usize,
                        )?;
                        depth += 1;
                        continue;
                    }
                    if !known(code) {
                        return Err(format!("an instruction has the unknown code {code}"));
                    }
                    let takes = arity(code);
                    if takes == 0 {
                        loads.push(code);
                    } else if depth < takes {
                        return Err("an instruction takes more than the stack holds".into());
                    }
                    depth = depth + 1 - takes;
                }
                if depth != 1 {
                    return Err("a cluster's node does not leave one value".into());
                }
                node += 1;
                at += 2 + len;
            }
            needs.push((writes, loads));
        }
        let clusters = fused.operand_at.len();
        let mut next = 0;
        for group in &fused.groups {
            let (writes, loads) = needs
                .get(group.shape as usize)
                .ok_or("a group's shape is not one of the clusters'")?;
            if group.first as usize != next || group.count as usize > fused.widest {
                return Err("the clusters' groups do not follow one another".into());
            }
            next = (group.first as usize)
                .checked_add(group.count as usize)
                .filter(|&end| end <= clusters)
                .ok_or("a group is past the clusters")?;
            for c in group.first as usize..next {
                let at = fused.operand_at[c] as usize;
                let operands = fused
                    .operands
                    .get(at..at + loads.len() + writes)
                    .ok_or("a cluster's operands are past the end")?;
                for (&code, &x) in loads.iter().zip(operands) {
                    match code {
                        LOAD => within("a cluster's slot", x as usize, fused_slots)?,
                        _ => reads(code, x, shared)?,
                    }
                }
                for &slot in &operands[loads.len()..] {
                    within("a cluster's slot", slot as usize, fused_slots)?;
                }
            }
        }
        match next == clusters {
            true => Ok(()),
            false => Err("the clusters' groups do not cover the clusters".into()),
        }
    }
}
