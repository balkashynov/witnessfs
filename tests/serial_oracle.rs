//! The oracle is a plain map and never invokes WitnessFS validation code.
use anyhow::Result;
use std::collections::BTreeMap;
use witnessfs::{
    model::*,
    store::{Store, View},
};

#[derive(Clone)]
struct Plan {
    reads: Vec<String>,
    enumerate: bool,
    output: String,
    salt: u64,
}

fn oracle(state: &BTreeMap<String, Vec<u8>>, p: &Plan) -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let values: Vec<_> = p.reads.iter().map(|k| state.get(k)).collect();
    let names: Vec<_> = if p.enumerate {
        state.keys().cloned().collect()
    } else {
        vec![]
    };
    let observed = serde_json::to_vec(&(values, names, p.salt)).unwrap();
    let mut next = state.clone();
    next.insert(p.output.clone(), observed.clone());
    (observed, next)
}
fn execute(v: &mut View<'_>, p: &Plan) -> Result<Vec<u8>> {
    let mut values = Vec::new();
    for k in &p.reads {
        values.push(match v.lookup(ROOT, k.as_bytes(), k)? {
            Some(n) => Some(v.read(n.id, 0, MAX_IO_SIZE, k)?),
            None => None,
        });
    }
    let names: Vec<String> = if p.enumerate {
        v.children(ROOT, "/")?
            .into_iter()
            .map(|(n, _)| String::from_utf8(n).unwrap())
            .collect()
    } else {
        vec![]
    };
    let observed = serde_json::to_vec(&(values, names, p.salt))?;
    let node = match v.lookup(ROOT, p.output.as_bytes(), &p.output)? {
        Some(n) => n,
        None => v.create(
            ROOT,
            p.output.as_bytes(),
            Kind::File,
            0o644,
            0,
            0,
            vec![],
            &p.output,
        )?,
    };
    v.truncate(node.id, 0, &p.output)?;
    v.write(node.id, 0, &observed, &p.output)?;
    Ok(observed)
}
fn rand(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn generated_accepted_histories_match_independent_serial_execution() -> Result<()> {
    let mut seed = 0x71a8ec52u64;
    let mut accepted = 0;
    let mut rejected = 0;
    let mut unsafe_merges = 0;
    for case in 0..80 {
        let tmp = tempfile::tempdir()?;
        let mut store = Store::create(&tmp.path().join("db"))?;
        let base: BTreeMap<String, Vec<u8>> = BTreeMap::from([
            ("a".into(), b"one".to_vec()),
            ("b".into(), b"two".to_vec()),
            ("c".into(), b"three".to_vec()),
        ]);
        store.branch("seed")?;
        store.transact("seed", |v| {
            for (k, data) in &base {
                let n = v.create(ROOT, k.as_bytes(), Kind::File, 0o644, 0, 0, vec![], k)?;
                v.write(n.id, 0, data, k)?;
            }
            Ok(())
        })?;
        store.seal("seed")?;
        store.accept("seed")?;
        let mut pending = vec![];
        for i in 0..8 {
            let id = format!("branch-{i}");
            store.branch(&id)?;
            let reads = (0..(rand(&mut seed) % 4))
                .map(|_| ["a", "b", "c", "missing"][(rand(&mut seed) % 4) as usize].into())
                .collect();
            let p = Plan {
                reads,
                enumerate: rand(&mut seed).is_multiple_of(5),
                output: if rand(&mut seed).is_multiple_of(3) {
                    ["a", "b", "c"][(rand(&mut seed) % 3) as usize].into()
                } else {
                    format!("result-{i}")
                },
                salt: rand(&mut seed),
            };
            let observed = store.transact(&id, |v| execute(v, &p))?;
            assert_eq!(observed, oracle(&base, &p).0);
            store.seal(&id)?;
            pending.push((id, p, observed));
        }
        for i in (1..pending.len()).rev() {
            let j = (rand(&mut seed) as usize) % (i + 1);
            pending.swap(i, j);
        }
        let mut serial = base.clone();
        let mut blind_merge = base;
        for (id, p, observed) in pending {
            if oracle(&blind_merge, &p).0 != observed {
                unsafe_merges += 1;
            }
            blind_merge.insert(p.output.clone(), observed.clone());
            let receipt = store.accept(&id)?;
            if receipt.outcome == "accepted" {
                let (expected, next) = oracle(&serial, &p);
                assert_eq!(
                    observed, expected,
                    "accepted stale observation in case {case}, {id}"
                );
                serial = next;
                accepted += 1;
            } else {
                rejected += 1;
            }
        }
        let actual = store.inspect(None, |v| {
            let mut map = BTreeMap::new();
            for (name, n) in v.children(ROOT, "/")? {
                map.insert(
                    String::from_utf8(name)?,
                    v.read(n.id, 0, MAX_IO_SIZE, "verify")?,
                );
            }
            Ok(map)
        })?;
        assert_eq!(actual, serial, "final state mismatch in case {case}");
    }
    assert!(accepted > 80 && rejected > 80 && unsafe_merges > 80);
    eprintln!(
        "serial oracle: 80 histories, 640 transactions, {accepted} accepted, {rejected} rejected; blind composition changed {unsafe_merges} observations"
    );
    Ok(())
}
