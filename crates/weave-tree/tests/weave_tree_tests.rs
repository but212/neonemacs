use proptest::prelude::*;
use weave_tree::{
    Cursor, DocError, LineBreakMetric, LineBreaks, MAX_SLICE_BYTES, Replace, Slices, Summary, Unit,
    Units, Utf16CodeUnits, Utf16Metric, WeaveTree, WeaveTreeBuilder,
};

fn collect_slices<S: Summary>(mut it: Slices<'_, S>) -> Vec<u8> {
    let mut out = Vec::new();
    while let (next, Some(chunk)) = it.next() {
        it = next;
        out.extend_from_slice(chunk);
    }
    out
}

fn collect_slices_rev<S: Summary>(mut it: Slices<'_, S>) -> Vec<u8> {
    let mut chunks = Vec::new();
    while let (next, Some(chunk)) = it.next_back() {
        it = next;
        chunks.push(chunk);
    }
    chunks.reverse();
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend_from_slice(chunk);
    }
    out
}

fn collect_units<S: Summary>(mut it: Units<'_, S>) -> Vec<Unit> {
    let mut out = Vec::new();
    while let (next, Some(u)) = it.next() {
        it = next;
        out.push(u);
    }
    out
}

fn collect_units_rev<S: Summary>(mut it: Units<'_, S>) -> Vec<Unit> {
    let mut out = Vec::new();
    while let (next, Some(u)) = it.next_back() {
        it = next;
        out.push(u);
    }
    out.reverse();
    out
}

#[test]
fn unit_summary_and_utf16_metrics_work() {
    let text = "a\né\n😀b";
    // Bytes: a(1) \n(1) é(2) \n(1) 😀(4) b(1) = 10
    // UTF-16: a(1) \n(1) é(1) \n(1) 😀(2) b(1) = 7
    let t_byte: WeaveTree = WeaveTree::from_bytes(text.as_bytes());
    assert_eq!(t_byte.total_bytes(), 10);
    assert_eq!(t_byte.summary(), ());

    let t_utf16: WeaveTree<Utf16CodeUnits> = WeaveTree::from_bytes(text.as_bytes());
    assert_eq!(t_utf16.total_metric::<Utf16Metric>(), 7);
    assert_eq!(t_utf16.byte_to_metric::<Utf16Metric>(0), Ok(0));
    assert_eq!(t_utf16.byte_to_metric::<Utf16Metric>(5), Ok(4)); // before 😀
    assert_eq!(t_utf16.byte_to_metric::<Utf16Metric>(9), Ok(6)); // after 😀
    assert_eq!(t_utf16.byte_to_metric::<Utf16Metric>(10), Ok(7));
    assert_eq!(t_utf16.metric_to_byte::<Utf16Metric>(4), Ok(5));
    assert_eq!(t_utf16.metric_to_byte::<Utf16Metric>(6), Ok(9));
    assert_eq!(
        t_utf16.metric_to_byte::<Utf16Metric>(8),
        Err(DocError::OutOfBounds {
            offset: 8,
            len: 0,
            total: 7
        })
    );
}

#[test]
fn cursor_and_iterators_traverse_multi_slice_tree() {
    let mut raw = Vec::new();
    for i in 0..(MAX_SLICE_BYTES + 200) {
        if i % 100 == 99 {
            raw.push(b'\n');
        } else {
            raw.push(b'a' + (i % 26) as u8);
        }
    }
    // Append multi-byte UTF-8 and raw invalid byte.
    raw.extend_from_slice("한글😀".as_bytes());
    raw.push(0xFF);

    let tree: WeaveTree<LineBreaks> = WeaveTreeBuilder::new()
        .push_bytes(&raw[..5000])
        .push_bytes(&raw[5000..])
        .finish();
    tree.assert_invariants();

    // Forward & reverse Slices stream
    assert_eq!(collect_slices(tree.slices()), raw);
    assert_eq!(collect_slices_rev(tree.slices()), raw);

    // Double-ended Slices meeting in the middle
    let mut it = tree.slices();
    let mut left_acc = Vec::new();
    let mut right_acc = Vec::new();
    while let (next, Some(front)) = it.next() {
        left_acc.extend_from_slice(front);
        let (next, back) = next.next_back();
        it = next;
        if let Some(back) = back {
            right_acc.push(back.to_vec());
        }
    }
    right_acc.reverse();
    for chunk in right_acc {
        left_acc.extend_from_slice(&chunk);
    }
    assert_eq!(left_acc, raw);

    // Units forward & reverse
    let fwd_units = collect_units(tree.units());
    let rev_units = collect_units_rev(tree.units());
    assert_eq!(fwd_units, rev_units);
    assert_eq!(fwd_units.last(), Some(&Unit::Raw(0xFF)));

    // Cursor seek_metric & prev_unit / next_unit
    let cur = Cursor::new(&tree);
    let total_breaks = tree.total_metric::<LineBreakMetric>();
    assert!(total_breaks > 10);
    let cur = cur.seek_metric::<LineBreakMetric>(5).unwrap();
    assert_eq!(cur.metric_offset::<LineBreakMetric>(), 5);
    let pos_5 = cur.byte_offset();
    let (cur, u) = cur.next_unit();
    let u = u.unwrap();
    let (cur, prev_u) = cur.prev_unit();
    assert_eq!(prev_u, Some(u));
    assert_eq!(cur.byte_offset(), pos_5);
}

proptest! {
    #[test]
    fn proptest_summary_monoid_and_utf16_edits(
        init in proptest::collection::vec(any::<u8>(), 0..512),
        ops in proptest::collection::vec((0usize..512, 0usize..64, proptest::collection::vec(any::<u8>(), 0..64)), 1..20)
    ) {
        let mut tree: WeaveTree<Utf16CodeUnits> = WeaveTree::from_bytes(&init);
        let mut oracle = init;

        for (raw_off, raw_del, ins) in ops {
            let off = tree.floor_boundary(raw_off.min(tree.total_bytes()));
            let end = tree.ceil_boundary((off + raw_del).min(tree.total_bytes()));
            let del = end - off;

            let (next, inv) = tree.clone().replace(off, del, &ins).unwrap();
            next.assert_invariants();
            oracle.splice(off..off + del, ins.iter().copied());

            let collected = collect_slices(next.slices());
            prop_assert_eq!(&collected, &oracle);

            // S-01: root summary equals from_slice on flattened bytes (no split_pair mismatch)
            let expected_utf16 = Utf16CodeUnits::from_slice(&oracle);
            prop_assert_eq!(next.summary(), expected_utf16);

            // D-04: undo inverse restores exact tree
            let (restored, _) = next.clone().apply(inv).unwrap();
            prop_assert_eq!(restored, tree);

            // Metric roundtrip on valid boundaries (re-snap after D-01 seam merge)
            let snapped = next.floor_boundary(off);
            let m = next.byte_to_metric::<Utf16Metric>(snapped).unwrap();
            prop_assert_eq!(next.metric_to_byte::<Utf16Metric>(m).unwrap(), snapped);

            tree = next;
        }
    }

    #[test]
    fn proptest_slices_and_units_range(
        data in proptest::collection::vec(any::<u8>(), 0..400),
        a in 0usize..400,
        b in 0usize..400,
    ) {
        let tree: WeaveTree<LineBreaks> = WeaveTree::from_bytes(&data);
        let lo = tree.floor_boundary(a.min(b).min(tree.total_bytes()));
        let hi = tree.ceil_boundary(a.max(b).min(tree.total_bytes()));

        let sub_bytes = collect_slices(tree.slices_range(lo..hi).unwrap());
        prop_assert_eq!(&sub_bytes, &data[lo..hi]);

        let fwd = collect_units(tree.units_range(lo..hi).unwrap());
        let rev = collect_units_rev(tree.units_range(lo..hi).unwrap());
        prop_assert_eq!(fwd, rev);

        // Replace<LineBreaks> smoke check
        let (t2, inv) = tree.clone().apply(Replace { offset: lo, delete_len: hi - lo, insert: WeaveTree::default() }).unwrap();
        let (t3, _) = t2.apply(inv).unwrap();
        prop_assert_eq!(t3, tree);
    }
}
