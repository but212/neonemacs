use core::ops::ControlFlow;
use proptest::prelude::*;
use weave_tree::{DocError, LineBreaks, MAX_SLICE_BYTES, Replace, Summary, WeaveTree, WeaveTreeBuilder};

fn collect_range(tree: &WeaveTree<LineBreaks>, range: core::ops::Range<usize>) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = tree.try_for_each_slice(range, |chunk| {
        out.extend_from_slice(chunk);
        ControlFlow::<()>::Continue(())
    })
    .unwrap();
    out
}

fn collect_range_rev(tree: &WeaveTree<LineBreaks>, range: core::ops::Range<usize>) -> Vec<u8> {
    let mut chunks = Vec::new();
    let _ = tree.try_for_each_slice_rev(range, |chunk| {
        chunks.push(chunk.to_vec());
        ControlFlow::<()>::Continue(())
    })
    .unwrap();
    chunks.reverse();
    chunks.concat()
}

#[test]
fn line_breaks_metrics_and_bounds_work() {
    let text = "a\né\n😀b";
    let tree: WeaveTree<LineBreaks> = WeaveTree::from_bytes(text.as_bytes());
    assert_eq!(tree.total_bytes(), 10);
    assert_eq!(tree.total_metric(), 2);
    assert_eq!(tree.byte_to_metric(0), Ok(0));
    assert_eq!(tree.byte_to_metric(2), Ok(1));
    assert_eq!(tree.byte_to_metric(10), Ok(2));
    assert_eq!(tree.metric_to_byte(0), Ok(0));
    assert_eq!(tree.metric_to_byte(1), Ok(2));
    assert_eq!(tree.metric_to_byte(2), Ok(5));
    assert_eq!(
        tree.metric_to_byte(3),
        Err(DocError::OutOfBounds {
            offset: 3,
            len: 0,
            total: 2,
        })
    );
}

#[test]
fn streaming_traverses_multi_slice_tree() {
    let mut raw = Vec::new();
    for i in 0..(MAX_SLICE_BYTES + 200) {
        if i % 100 == 99 {
            raw.push(b'\n');
        } else {
            raw.push(b'a' + (i % 26) as u8);
        }
    }
    raw.extend_from_slice("한글😀".as_bytes());
    raw.push(0xFF);

    let tree: WeaveTree<LineBreaks> = WeaveTreeBuilder::new()
        .push_bytes(&raw[..5000])
        .push_bytes(&raw[5000..])
        .finish();
    tree.assert_invariants();

    assert_eq!(collect_range(&tree, 0..tree.total_bytes()), raw);
    assert_eq!(collect_range_rev(&tree, 0..tree.total_bytes()), raw);
    assert!(tree.total_metric() > 10);
}

proptest! {
    #[test]
    fn proptest_summary_monoid_and_edits(
        init in proptest::collection::vec(any::<u8>(), 0..512),
        ops in proptest::collection::vec((0usize..512, 0usize..64, proptest::collection::vec(any::<u8>(), 0..64)), 1..20)
    ) {
        let mut tree: WeaveTree<LineBreaks> = WeaveTree::from_bytes(&init);
        let mut oracle = init;

        for (raw_off, raw_del, ins) in ops {
            let off = tree.floor_boundary(raw_off.min(tree.total_bytes()));
            let end = tree.ceil_boundary((off + raw_del).min(tree.total_bytes()));
            let del = end - off;

            let (next, inv) = tree.clone().replace(off, del, &ins).unwrap();
            next.assert_invariants();
            oracle.splice(off..off + del, ins.iter().copied());

            let collected = collect_range(&next, 0..next.total_bytes());
            prop_assert_eq!(&collected, &oracle);

            let expected_lines = LineBreaks::from_slice(&oracle);
            prop_assert_eq!(next.total_metric(), expected_lines.measure());

            let (restored, _) = next.clone().apply(inv).unwrap();
            prop_assert_eq!(restored, tree);

            let snapped = next.floor_boundary(off);
            let m = next.byte_to_metric(snapped).unwrap();
            let line_start = next.metric_to_byte(m).unwrap();
            prop_assert!(line_start <= snapped);

            tree = next;
        }
    }

    #[test]
    fn proptest_slice_range_roundtrip(
        data in proptest::collection::vec(any::<u8>(), 0..400),
        a in 0usize..400,
        b in 0usize..400,
    ) {
        let tree: WeaveTree<LineBreaks> = WeaveTree::from_bytes(&data);
        let lo = tree.floor_boundary(a.min(b).min(tree.total_bytes()));
        let hi = tree.ceil_boundary(a.max(b).min(tree.total_bytes()));

        prop_assert_eq!(&collect_range(&tree, lo..hi), &data[lo..hi]);
        prop_assert_eq!(&collect_range_rev(&tree, lo..hi), &data[lo..hi]);

        let (t2, inv) = tree.clone().apply(Replace { offset: lo, delete_len: hi - lo, insert: WeaveTree::default() }).unwrap();
        let (t3, _) = t2.apply(inv).unwrap();
        prop_assert_eq!(t3, tree);
    }
}
