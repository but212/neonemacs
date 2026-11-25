use std::ops::ControlFlow;

use neonemacs_core::{
    Doc, DocBuilder, DocError, MAX_SLICE_BYTES, Replace, Unit, line_to_offset, offset_to_line,
    total_lines,
};
use proptest::prelude::*;

fn collect_bytes(doc: &Doc) -> Vec<u8> {
    let mut out = Vec::with_capacity(doc.total_bytes());
    let res = doc
        .try_for_each_slice(0..doc.total_bytes(), |s| {
            out.extend_from_slice(s);
            ControlFlow::<()>::Continue(())
        })
        .expect("full range is valid");
    assert_eq!(res, ControlFlow::Continue(()));
    out
}

fn collect_bytes_rev(doc: &Doc) -> Vec<u8> {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let res = doc
        .try_for_each_slice_rev(0..doc.total_bytes(), |s| {
            chunks.push(s.to_vec());
            ControlFlow::<()>::Continue(())
        })
        .expect("full range is valid");
    assert_eq!(res, ControlFlow::Continue(()));
    chunks.reverse();
    chunks.into_iter().flatten().collect()
}

fn oracle_total_lines(bytes: &[u8]) -> usize {
    1 + bytes.iter().filter(|&&b| b == b'\n').count()
}

fn oracle_line_to_offset(bytes: &[u8], line: usize) -> Option<usize> {
    if line == 0 {
        return Some(0);
    }
    let mut seen = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line {
                return Some(i + 1);
            }
        }
    }
    None
}

fn oracle_offset_to_line(bytes: &[u8], offset: usize) -> usize {
    bytes[..offset].iter().filter(|&&b| b == b'\n').count()
}

#[test]
fn empty_doc_contracts() {
    let doc = Doc::default();
    assert_eq!(doc, Doc::from_bytes(b""));
    assert_eq!(doc.total_bytes(), 0);
    assert_eq!(total_lines(&doc), 1);
    assert_eq!(line_to_offset(&doc, 0), Ok(0));
    assert_eq!(
        line_to_offset(&doc, 1),
        Err(DocError::OutOfBounds {
            offset: 1,
            len: 0,
            total: 0
        })
    );
    assert_eq!(offset_to_line(&doc, 0), Ok(0));
    assert_eq!(doc.byte_at(0), None);
    assert_eq!(doc.decode_at(0), None);
    assert_eq!(doc.floor_boundary(10), 0);
    assert_eq!(doc.ceil_boundary(10), 0);
    assert_eq!(doc.prev_boundary(10), 0);
    assert_eq!(doc.next_boundary(10), 0);
}

#[test]
fn shared_replacements_reject_unrepresentable_lengths() {
    let mut doc = Doc::from_bytes(&[b'x'; MAX_SLICE_BYTES]);
    while doc.total_bytes() <= usize::MAX / 2 {
        let insert = doc.clone();
        let offset = doc.total_bytes();
        (doc, _) = doc
            .apply(Replace {
                offset,
                delete_len: 0,
                insert,
            })
            .unwrap();
    }
    let n = doc.total_bytes();
    // Neither `usize::MAX` bytes (no room for one-past-end) nor an overflowing sum is valid.
    for insert in [doc.slice(0..n - 1).unwrap(), doc.clone()] {
        let result = doc.clone().apply(Replace {
            offset: n,
            delete_len: 0,
            insert,
        });
        assert_eq!(
            result.unwrap_err(),
            (doc.clone(), DocError::DocumentTooLarge)
        );
    }
}

#[test]
fn error_paths_return_original_doc() {
    let doc = Doc::from_bytes("가나다".as_bytes());
    let (doc, err) = doc.replace(1, 0, b"x").expect_err("mid-char boundary");
    assert_eq!(err, DocError::InvalidBoundary { offset: 1 });
    assert_eq!(collect_bytes(&doc), "가나다".as_bytes());

    let (doc, err) = doc
        .replace(usize::MAX, 1, b"x")
        .expect_err("overflow out of bounds");
    assert_eq!(
        err,
        DocError::OutOfBounds {
            offset: usize::MAX,
            len: 1,
            total: 9
        }
    );
    assert_eq!(collect_bytes(&doc), "가나다".as_bytes());

    assert_eq!(
        offset_to_line(&doc, 2),
        Err(DocError::InvalidBoundary { offset: 2 })
    );
    assert_eq!(
        offset_to_line(&doc, 10),
        Err(DocError::OutOfBounds {
            offset: 10,
            len: 0,
            total: 9
        })
    );
}

#[test]
fn streaming_empty_and_break() {
    let doc = Doc::from_bytes(b"hello\nworld");
    let mut calls = 0usize;
    let res = doc
        .try_for_each_slice(5..5, |_| {
            calls += 1;
            ControlFlow::<()>::Continue(())
        })
        .expect("empty range valid");
    assert_eq!(res, ControlFlow::Continue(()));
    assert_eq!(calls, 0);

    let res = doc
        .try_for_each_slice(0..doc.total_bytes(), |s| ControlFlow::Break(s[0]))
        .expect("valid range");
    assert_eq!(res, ControlFlow::Break(b'h'));

    let res = doc
        .try_for_each_slice_rev(0..doc.total_bytes(), |s| {
            ControlFlow::Break(*s.last().expect("non-empty"))
        })
        .expect("valid range");
    assert_eq!(res, ControlFlow::Break(b'd'));
}

#[test]
fn rfc3629_strict_utf8_and_raw_preservation() {
    // Overlong, surrogate U+D800, > U+10FFFF, valid 'é'
    let raw = [
        0xC0, 0xAF, 0xED, 0xA0, 0x80, 0xF4, 0x90, 0x80, 0x80, 0xC3, 0xA9,
    ];
    let doc = Doc::from_bytes(&raw);
    assert_eq!(doc.decode_at(0), Some((Unit::Raw(0xC0), 1)));
    assert_eq!(doc.decode_at(1), Some((Unit::Raw(0xAF), 1)));
    assert_eq!(doc.decode_at(2), Some((Unit::Raw(0xED), 1)));
    assert_eq!(doc.decode_at(5), Some((Unit::Raw(0xF4), 1)));
    assert_eq!(doc.decode_at(9), Some((Unit::Char('é'), 2)));
    assert_eq!(doc.decode_at(10), None);
    assert_eq!(doc.floor_boundary(10), 9);
    assert_eq!(doc.ceil_boundary(10), 11);
    assert_eq!(doc.prev_boundary(11), 9);
    assert_eq!(doc.next_boundary(9), 11);
}

#[test]
fn seam_merge_3way_and_independent_2way_and_undo() {
    // 3-way bridge: [0xF0, 0x9F] + [0x98] + [0x80] -> '😀'
    let doc = Doc::from_bytes(&[0xF0, 0x9F, b'X', 0x80]);
    let (next, inv) = doc.clone().replace(2, 1, &[0x98]).expect("3-way merge");
    assert_eq!(collect_bytes(&next), "😀".as_bytes());
    assert_eq!(next.decode_at(0), Some((Unit::Char('😀'), 4)));
    let (restored, _) = next.apply(inv).expect("D-04 round-trip");
    assert_eq!(restored, doc);

    // Independent 2-way: [0xC3, 0xC3] + [0xA9] -> [Raw(0xC3), Char('é')]
    let doc2 = Doc::from_bytes(&[0xC3, 0xC3]);
    let (next2, inv2) = doc2.clone().replace(2, 0, &[0xA9]).expect("2-way merge");
    assert_eq!(next2.decode_at(0), Some((Unit::Raw(0xC3), 1)));
    assert_eq!(next2.decode_at(1), Some((Unit::Char('é'), 2)));
    let (restored2, _) = next2.apply(inv2).expect("D-04 round-trip");
    assert_eq!(restored2, doc2);
}

#[test]
fn multi_level_tree_height_3_and_split_join() {
    // Force > 32 * 32 slices using > 1024 * MAX_SLICE_BYTES? No, build via small alternating
    // inserts or > 33 * MAX_SLICE_BYTES (~540 KB) for height 2, and test split/join across it.
    let big = vec![b'a'; MAX_SLICE_BYTES * 40];
    let doc = Doc::from_bytes(&big);
    assert_eq!(doc.total_bytes(), big.len());
    let (doc2, inv) = doc
        .clone()
        .replace(MAX_SLICE_BYTES * 20, 123, b"hello\nworld")
        .expect("edit");
    assert_eq!(total_lines(&doc2), 2);
    let (doc3, _) = doc2.apply(inv).expect("undo");
    assert_eq!(doc3, doc);
}

#[test]
fn doc_builder_preserves_multibyte_utf8_across_chunk_boundaries() {
    // 2-byte 'é' split across 2 chunks
    let doc = DocBuilder::new()
        .push_bytes(&[0xC3])
        .push_bytes(&[0xA9])
        .finish();
    #[cfg(debug_assertions)]
    doc.assert_invariants();
    assert_eq!(doc.decode_at(0), Some((Unit::Char('é'), 2)));
    assert!(!doc.is_boundary(1));
    assert_eq!(
        doc.clone().replace(1, 0, b"x").expect_err("mid-char"),
        (doc, DocError::InvalidBoundary { offset: 1 })
    );

    // 4-byte '😀' split 1 byte at a time
    let emoji = DocBuilder::new()
        .push_bytes(&[0xF0])
        .push_bytes(&[0x9F])
        .push_bytes(&[0x98])
        .push_bytes(&[0x80])
        .finish();
    #[cfg(debug_assertions)]
    emoji.assert_invariants();
    assert_eq!(emoji.decode_at(0), Some((Unit::Char('😀'), 4)));
    assert!(!emoji.is_boundary(1));
    assert!(!emoji.is_boundary(2));
    assert!(!emoji.is_boundary(3));

    // Trailing incomplete prefix at EOF + invalid prefix followed by valid UTF-8
    let mixed = DocBuilder::new()
        .push_bytes(&[0xF0, 0x9F])
        .push_bytes(&[0xC3, 0xA9, 0xEA])
        .finish();
    #[cfg(debug_assertions)]
    mixed.assert_invariants();
    assert_eq!(mixed.decode_at(0), Some((Unit::Raw(0xF0), 1)));
    assert_eq!(mixed.decode_at(1), Some((Unit::Raw(0x9F), 1)));
    assert_eq!(mixed.decode_at(2), Some((Unit::Char('é'), 2)));
    assert_eq!(mixed.decode_at(4), Some((Unit::Raw(0xEA), 1)));

    // 64 KiB chunk split inside 3-byte Hangul '가'
    let hangul = "가".repeat(22_000);
    let raw = hangul.as_bytes();
    let mut b = DocBuilder::new();
    for chunk in raw.chunks(64 * 1024) {
        b = b.push_bytes(chunk);
    }
    let built = b.finish();
    #[cfg(debug_assertions)]
    built.assert_invariants();
    assert_eq!(built.decode_at(65_535), Some((Unit::Char('가'), 3)));
    assert!(!built.is_boundary(65_536));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn prop_doc_builder_matches_from_bytes_across_arbitrary_chunks(
        chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..32), 0..48),
    ) {
        let mut flat = Vec::new();
        let mut builder = DocBuilder::new();
        for c in &chunks {
            flat.extend_from_slice(c);
            builder = builder.push_bytes(c);
        }
        let built = builder.finish();
        #[cfg(debug_assertions)]
        built.assert_invariants();
        let expected = Doc::from_bytes(&flat);
        prop_assert_eq!(&built, &expected);
        for off in 0..=flat.len() {
            prop_assert_eq!(built.is_boundary(off), expected.is_boundary(off));
            prop_assert_eq!(built.decode_at(off), expected.decode_at(off));
        }
    }

    #[test]
    fn prop_random_edits_and_undo_and_queries(
        init in prop::collection::vec(any::<u8>(), 0..256),
        ops in prop::collection::vec((0usize..512, 0usize..64, prop::collection::vec(any::<u8>(), 0..64)), 1..32),
    ) {
        let mut doc = Doc::from_bytes(&init);
        let mut oracle = init;
        let mut history: Vec<(Doc, Replace)> = Vec::new();

        for (raw_off, raw_del, ins) in ops {
            let off = doc.floor_boundary(raw_off % (doc.total_bytes() + 1));
            let max_del = doc.total_bytes() - off;
            let del = doc.floor_boundary(off + (raw_del % (max_del + 1))) - off;

            let prev_doc = doc.clone();
            let (next_doc, inv) = doc.replace(off, del, &ins).expect("snapped edit succeeds");
            oracle.splice(off..off + del, ins.iter().copied());

            prop_assert_eq!(next_doc.total_bytes(), oracle.len());
            prop_assert_eq!(total_lines(&next_doc), oracle_total_lines(&oracle));
            prop_assert_eq!(collect_bytes(&next_doc), oracle.clone());
            prop_assert_eq!(collect_bytes_rev(&next_doc), oracle.clone());
            prop_assert_eq!(&next_doc, &Doc::from_bytes(&oracle));

            // Verify coordinate mappings at sampled lines and offsets.
            let line_count = total_lines(&next_doc);
            let sample_line = off % line_count;
            prop_assert_eq!(
                line_to_offset(&next_doc, sample_line),
                Ok(oracle_line_to_offset(&oracle, sample_line).unwrap())
            );
            let q_off = next_doc.floor_boundary(off.min(next_doc.total_bytes()));
            prop_assert_eq!(
                offset_to_line(&next_doc, q_off),
                Ok(oracle_offset_to_line(&oracle, q_off))
            );

            history.push((prev_doc, inv));
            doc = next_doc;
        }

        // Unwind all undo operations (D-04).
        for (expected_prev, inv) in history.into_iter().rev() {
            let (restored, _) = doc.apply(inv).expect("D-04 inverse must succeed");
            prop_assert_eq!(&restored, &expected_prev);
            doc = restored;
        }
    }

    #[test]
    fn prop_apply_with_tree_insert_and_asymmetric_splits(
        left_chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..32), 1..40),
        right_chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..32), 1..40),
        split_ratio in 0usize..100,
    ) {
        // Build two multi-leaf trees incrementally via apply(Replace) so tree has varied structure.
        let mut d1 = Doc::default();
        for c in &left_chunks {
            let pos = d1.total_bytes();
            let ins = Doc::from_bytes(c);
            let (next, _) = d1.apply(Replace { offset: pos, delete_len: 0, insert: ins }).expect("append");
            d1 = next;
        }
        let mut d2 = Doc::default();
        for c in &right_chunks {
            let pos = d2.total_bytes();
            let ins = Doc::from_bytes(c);
            let (next, _) = d2.apply(Replace { offset: pos, delete_len: 0, insert: ins }).expect("append");
            d2 = next;
        }

        let pos = if d1.total_bytes() == 0 {
            0
        } else {
            d1.floor_boundary((d1.total_bytes() * split_ratio) / 100)
        };
        let del = d1.floor_boundary((pos + 17).min(d1.total_bytes())) - pos;
        let orig_bytes = collect_bytes(&d1);
        let d2_bytes = collect_bytes(&d2);
        let mut expected = orig_bytes.clone();
        expected.splice(pos..pos + del, d2_bytes);

        let (merged, inv) = d1.clone().apply(Replace { offset: pos, delete_len: del, insert: d2 }).expect("apply");
        #[cfg(debug_assertions)]
        merged.assert_invariants();
        prop_assert_eq!(collect_bytes(&merged), expected.clone());
        prop_assert_eq!(&merged, &Doc::from_bytes(&expected));

        let (restored, _) = merged.apply(inv).expect("undo");
        #[cfg(debug_assertions)]
        restored.assert_invariants();
        prop_assert_eq!(restored, d1);
    }
}
