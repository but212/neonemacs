use std::hint::black_box;
use std::ops::ControlFlow;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use weave_tree::{LineBreaks, WeaveTree, WeaveTreeBuilder};

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

fn bench_weave_tree_core(c: &mut Criterion) {
    let mut group = c.benchmark_group("weave_tree_core");

    let base_100k = make_lines_doc(100 * 1024, 80);

    // Pure in-place append fast-path (Arc::get_mut) across 16 consecutive keystrokes
    group.bench_function(BenchmarkId::new("edit_in_place", "insert_16_100kb"), |b| {
        b.iter_batched(
            || {
                base_100k
                    .clone()
                    .replace(50 * 1024, 0, b"z")
                    .expect("valid split")
                    .0
            },
            |mut doc| {
                for i in 0..16 {
                    let (next, _) = doc
                        .replace(50 * 1024 + 1 + i, 0, b"z")
                        .expect("valid in-place append");
                    doc = next;
                }
                black_box(doc)
            },
            BatchSize::SmallInput,
        );
    });

    // Pure in-place tail delete fast-path (Arc::get_mut) across 16 consecutive backspaces
    let pad32 = vec![b'q'; 32];
    group.bench_function(BenchmarkId::new("edit_in_place", "delete_16_100kb"), |b| {
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

    // ~2000 slices (height 3 with MAX_FANOUT=32) to exercise split_at/join O(log P) scaling
    let deep_doc = make_fragmented_doc(2000, 1200);
    let mid = deep_doc.total_bytes() / 2;

    group.bench_function("cow_split_join_deep_2000_slices", |b| {
        b.iter_batched(
            || deep_doc.clone(),
            |doc| {
                let (next, inv) = doc.replace(mid, 1, b"xy").expect("valid");
                black_box((next, inv))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("slice_deep_2000_slices", |b| {
        b.iter(|| {
            let sub = deep_doc.slice(mid - 500..mid + 500).expect("valid slice");
            black_box(sub)
        });
    });

    let raw_1mb = make_lines_bytes(1024 * 1024, 80);
    let doc_1mb = Doc::from_bytes(&raw_1mb);

    group.bench_function(BenchmarkId::new("build_1mb", "from_bytes"), |b| {
        b.iter(|| black_box(Doc::from_bytes(black_box(&raw_1mb))));
    });

    group.bench_function(BenchmarkId::new("build_1mb", "streaming_64kb_chunks"), |b| {
        b.iter(|| {
            let mut builder = WeaveTreeBuilder::<LineBreaks>::new();
            for chunk in black_box(&raw_1mb).chunks(64 * 1024) {
                builder = builder.push_bytes(chunk);
            }
            black_box(builder.finish())
        });
    });

    group.bench_function("scan_1mb", |b| {
        b.iter(|| {
            let mut total = 0usize;
            let _ = doc_1mb
                .try_for_each_slice(0..doc_1mb.total_bytes(), |chunk| {
                    total += chunk.len();
                    ControlFlow::<()>::Continue(())
                })
                .expect("valid range");
            black_box(total)
        });
    });

    let mid_line = doc_1mb.total_metric().div_ceil(2);
    let mid_offset = doc_1mb.metric_to_byte(mid_line).expect("valid line");

    group.bench_function(BenchmarkId::new("metric_lookup_1mb", "line_to_offset"), |b| {
        b.iter(|| {
            let off = doc_1mb
                .metric_to_byte(black_box(mid_line))
                .expect("valid line");
            black_box(off)
        });
    });

    group.bench_function(BenchmarkId::new("metric_lookup_1mb", "offset_to_line"), |b| {
        b.iter(|| {
            let line = doc_1mb
                .byte_to_metric(black_box(mid_offset))
                .expect("valid offset");
            black_box(line)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_weave_tree_core);
criterion_main!(benches);
