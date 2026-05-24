use criterion::{Criterion, criterion_group, criterion_main};
use path_tracer::render::scene::Scene;
use std::hint::black_box;

fn benchmark_scene_loading(c: &mut Criterion) {
    c.bench_function("scene_new_dorm", |b| {
        b.iter(|| {
            pollster::block_on(Scene::new(black_box("assets/dorm.glb")))
                .expect("Failed to load scene")
        });
    });
}

criterion_group!(benches, benchmark_scene_loading);
criterion_main!(benches);
