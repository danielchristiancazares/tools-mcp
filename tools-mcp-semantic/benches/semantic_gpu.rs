use criterion::{Criterion, criterion_group, criterion_main};

#[path = "semantic.rs"]
mod semantic;

fn run_all(c: &mut Criterion) {
    semantic::run_all(c);
}

criterion_group!(benches, run_all);
criterion_main!(benches);
