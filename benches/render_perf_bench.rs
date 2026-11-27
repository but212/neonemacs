use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use neonemacs::core::render as core_render;
use neonemacs::core::{Config, Editor};
use neonemacs::shell::render::{Cell, RenderScratch, frame, render_into};

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

fn bench_shell_render(c: &mut Criterion) {
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

    let mut grid_100k: Vec<Cell> = Vec::new();
    let mut scratch_100k = RenderScratch::default();
    group.bench_function("scroll_and_render_into_100kb_120x40", |b| {
        b.iter_batched(
            || ed_100k.clone(),
            |ed| {
                let scrolled = core_render::scroll(ed, 120, 40);
                let cursor = render_into(&scrolled, 120, 40, &mut grid_100k, &mut scratch_100k);
                black_box((cursor, grid_100k.len()))
            },
            BatchSize::SmallInput,
        );
    });

    let mut grid_long: Vec<Cell> = Vec::new();
    let mut scratch_long = RenderScratch::default();
    group.bench_function("scroll_and_render_into_long_line_120x40", |b| {
        b.iter_batched(
            || ed_long.clone(),
            |ed| {
                let scrolled = core_render::scroll(ed, 120, 40);
                let cursor = render_into(&scrolled, 120, 40, &mut grid_long, &mut scratch_long);
                black_box((cursor, grid_long.len()))
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("scroll_and_frame_100kb_120x40", |b| {
        b.iter_batched(
            || ed_100k.clone(),
            |ed| {
                let scrolled = core_render::scroll(ed, 120, 40);
                let f = frame(&scrolled, 120, 40);
                black_box(f)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("scroll_and_frame_long_line_120x40", |b| {
        b.iter_batched(
            || ed_long.clone(),
            |ed| {
                let scrolled = core_render::scroll(ed, 120, 40);
                let f = frame(&scrolled, 120, 40);
                black_box(f)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_shell_render);
criterion_main!(benches);
