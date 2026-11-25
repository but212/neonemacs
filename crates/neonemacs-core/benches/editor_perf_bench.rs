use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use neonemacs_core::render;
use neonemacs_core::{Config, Editor};

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

fn bench_editor_typing(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_typing");
    let raw_100k = make_lines_bytes(100 * 1024, 80);
    let base_ed = Editor::new(Config::default())
        .insert_at(0, &raw_100k)
        .move_point(50 * 1024)
        .boundary();

    group.bench_function("editor_self_insert_subsequent_100kb", |b| {
        b.iter_batched(
            || base_ed.clone().self_insert('a'),
            |ed| {
                let next = ed.self_insert('z');
                black_box(next)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("editor_self_insert_16_consecutive_100kb", |b| {
        b.iter_batched(
            || base_ed.clone(),
            |mut ed| {
                for _ in 0..16 {
                    ed = ed.self_insert('z');
                }
                black_box(ed)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_editor_delete(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_delete");
    let raw_100k = make_lines_bytes(100 * 1024, 80);
    let base_ed = Editor::new(Config::default())
        .insert_at(0, &raw_100k)
        .move_point(50 * 1024)
        .boundary();
    let pad32 = vec![b'q'; 32];

    group.bench_function("editor_backspace_16_consecutive_100kb", |b| {
        b.iter_batched(
            || {
                base_ed
                    .clone()
                    .insert_at(50 * 1024, &pad32)
                    .move_point(50 * 1024 + 32)
            },
            |mut ed| {
                for i in (0..16).rev() {
                    let pos = 50 * 1024 + 16 + i;
                    ed = ed.delete_only(pos, pos + 1);
                }
                black_box(ed)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_editor_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_render");
    let raw_100k = make_lines_bytes(100 * 1024, 80);
    let ed_100k = Editor::new(Config::default())
        .insert_at(0, &raw_100k)
        .move_point(50 * 1024)
        .boundary();

    let long_line = vec![b'a'; 20 * 1024];
    let ed_long = Editor::new(Config::default())
        .insert_at(0, &long_line)
        .move_point(10 * 1024)
        .boundary();

    group.bench_function("scroll_and_frame_100kb_120x40", |b| {
        b.iter_batched(
            || ed_100k.clone(),
            |ed| {
                let scrolled = render::scroll(ed, 120, 40);
                let frame = render::frame(&scrolled, 120, 40);
                black_box(frame)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("scroll_and_frame_long_line_120x40", |b| {
        b.iter_batched(
            || ed_long.clone(),
            |ed| {
                let scrolled = render::scroll(ed, 120, 40);
                let frame = render::frame(&scrolled, 120, 40);
                black_box(frame)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_editor_typing,
    bench_editor_delete,
    bench_editor_render
);
criterion_main!(benches);
