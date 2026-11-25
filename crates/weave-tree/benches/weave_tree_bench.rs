use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use weave_tree::{LineBreaks, WeaveTree};

type Doc = WeaveTree<LineBreaks>;

fn make_lines_bytes(total_bytes: usize, line_len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(total_bytes);
    let mut col = 0usize;
    while buf.len() < total_bytes {
        if col + 1 >= line_len {
            buf.push(b'\n');
            col = 0;
        } else {
            buf.push(b'a' + (col % 26) as u8);
            col += 1;
        }
    }
    buf
}

fn make_lines_doc(total_bytes: usize, line_len: usize) -> Doc {
    Doc::from_bytes(&make_lines_bytes(total_bytes, line_len))
}

fn make_fragmented_doc(slices: usize, slice_bytes: usize) -> Doc {
    let chunk = vec![b'x'; slice_bytes];
    let mut doc = Doc::default();
    for _ in 0..slices {
        let pos = doc.total_bytes();
        let (next, _) = doc.replace(pos, 0, &chunk).expect("valid replace");
        doc = next;
    }
    doc
}

fn bench_micro_edits(c: &mut Criterion) {
    let mut group = c.benchmark_group("weave_tree_micro_edits");

    let base_100k = make_lines_doc(100 * 1024, 80);

    group.bench_function("typing_single_char_100kb", |b| {
        b.iter_batched(
            || base_100k.clone(),
            |doc| {
                let (next, inv) = doc.replace(50 * 1024, 0, b"z").expect("valid replace");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("typing_16_consecutive_chars_100kb", |b| {
        b.iter_batched(
            || base_100k.clone(),
            |mut doc| {
                for pos in (50 * 1024..).take(16) {
                    let (next, _) = doc.replace(pos, 0, b"z").expect("valid replace");
                    doc = next;
                }
                black_box(doc)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("delete_then_insert_100kb", |b| {
        b.iter_batched(
            || base_100k.clone(),
            |doc| {
                let (after_del, _) = doc.replace(50 * 1024, 1, b"").expect("valid delete");
                let (after_ins, inv) = after_del.replace(50 * 1024, 0, b"q").expect("valid insert");
                black_box((after_ins, inv))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("typing_with_snapshot_clone_100kb", |b| {
        b.iter_batched(
            || base_100k.clone(),
            |doc| {
                let snapshot = doc.clone();
                let (next, _) = doc.replace(50 * 1024, 0, b"z").expect("valid replace");
                black_box((snapshot, next))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("script_3000_inserts", |b| {
        b.iter_batched(
            Doc::default,
            |mut doc| {
                for i in 0..3000 {
                    let (next, _) = doc.replace(i, 0, b"a").expect("valid insert");
                    doc = next;
                }
                black_box(doc)
            },
            BatchSize::SmallInput,
        );
    });

    // Subsequent keystroke after mid-slice split exercises in-place append via Arc::get_mut
    group.bench_function("mid_split_subsequent_char_100kb", |b| {
        b.iter_batched(
            || {
                base_100k
                    .clone()
                    .replace(50 * 1024, 0, b"z")
                    .expect("valid split")
                    .0
            },
            |doc| {
                let (next, inv) = doc
                    .replace(50 * 1024 + 1, 0, b"z")
                    .expect("valid in-place append");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    let pad32 = vec![b'q'; 32];
    group.bench_function("delete_subsequent_100kb", |b| {
        b.iter_batched(
            || {
                base_100k
                    .clone()
                    .replace(50 * 1024, 0, &pad32)
                    .expect("valid")
                    .0
            },
            |doc| {
                let (next, inv) = doc.replace(50 * 1024 + 31, 1, b"").expect("valid");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("delete_16_consecutive_100kb", |b| {
        b.iter_batched(
            || {
                base_100k
                    .clone()
                    .replace(50 * 1024, 0, &pad32)
                    .expect("valid")
                    .0
            },
            |mut doc| {
                for i in (0..16).rev() {
                    let (next, _) = doc.replace(50 * 1024 + 16 + i, 1, b"").expect("valid");
                    doc = next;
                }
                black_box(doc)
            },
            BatchSize::SmallInput,
        );
    });

    let small_100b = vec![b'a'; 100];
    group.bench_function("fresh_slice_first_append_100b", |b| {
        b.iter_batched(
            || Doc::from_bytes(&small_100b),
            |doc| {
                let (next, inv) = doc.replace(100, 0, b"z").expect("valid");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    let doc_1mb_edits = make_lines_doc(1024 * 1024, 80);
    let jump_offsets = [100_000, 900_000, 250_000, 750_000];
    group.bench_function("remote_jump_4_edits_1mb", |b| {
        b.iter_batched(
            || doc_1mb_edits.clone(),
            |mut doc| {
                for &off in &jump_offsets {
                    let (next, _) = doc.replace(off, 0, b"x").expect("valid jump edit");
                    doc = next;
                }
                black_box(doc)
            },
            BatchSize::SmallInput,
        );
    });

    let paste_64k = vec![b'p'; 64 * 1024];
    group.bench_function("paste_64kb_into_100kb", |b| {
        b.iter_batched(
            || base_100k.clone(),
            |doc| {
                let (next, inv) = doc.replace(50 * 1024, 0, &paste_64k).expect("valid paste");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_tree_and_lines(c: &mut Criterion) {
    let mut group = c.benchmark_group("weave_tree_structure_and_lines");

    // ~2000 slices (height 3 with MAX_FANOUT=32) to exercise split_at O(log^2 P) vs O(log P)
    let deep_doc = make_fragmented_doc(2000, 1200);
    let mid = deep_doc.total_bytes() / 2;

    group.bench_function("slice_deep_2000_slices", |b| {
        b.iter(|| {
            let sub = deep_doc.slice(mid - 500..mid + 500).expect("valid slice");
            black_box(sub)
        });
    });

    let raw_1mb = make_lines_bytes(1024 * 1024, 80);
    let doc_1mb = Doc::from_bytes(&raw_1mb);

    group.bench_function("from_bytes_1mb", |b| {
        b.iter(|| black_box(Doc::from_bytes(black_box(&raw_1mb))));
    });

    group.bench_function("cow_split_join_1mb", |b| {
        b.iter_batched(
            || doc_1mb.clone(),
            |doc| {
                let (next, inv) = doc.replace(512 * 1024, 1, b"xy").expect("valid");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("cow_deep_2000_slices", |b| {
        b.iter_batched(
            || deep_doc.clone(),
            |doc| {
                let (next, inv) = doc.replace(mid, 0, b"x").expect("valid");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    let mid_line = doc_1mb.total_metric().div_ceil(2);
    let mid_offset = doc_1mb
        .metric_to_byte(mid_line)
        .expect("valid line");

    group.bench_function("line_to_offset_1mb", |b| {
        b.iter(|| {
            let off = doc_1mb
                .metric_to_byte(black_box(mid_line))
                .expect("valid line");
            black_box(off)
        });
    });

    group.bench_function("offset_to_line_1mb", |b| {
        b.iter(|| {
            let line = doc_1mb
                .byte_to_metric(black_box(mid_offset))
                .expect("valid offset");
            black_box(line)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_micro_edits, bench_tree_and_lines);
criterion_main!(benches);
