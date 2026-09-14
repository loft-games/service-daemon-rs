use std::{hint::black_box, sync::Arc};

use criterion::Criterion;

use crate::core::managed_state::StateManager;

fn unexpected_init() -> std::future::Ready<Arc<u64>> {
    panic!("a warm provider must not invoke its initializer")
}

pub(super) fn run(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let expected = Arc::new(42u64);
    let immutable = StateManager::new();
    let managed = StateManager::new();

    runtime.block_on(async {
        let initial = immutable
            .resolve_snapshot(|| async { Arc::clone(&expected) })
            .await;
        assert!(Arc::ptr_eq(&initial, &expected));
        let initial = managed
            .resolve_snapshot(|| async { Arc::clone(&expected) })
            .await;
        assert!(Arc::ptr_eq(&initial, &expected));
        let tracked = managed.resolve_rwlock(unexpected_init).await;
        // Verify the promoted managed path, not just the immutable cache: publish
        // another Arc and require resolution to see it, then restore the fixture.
        let replacement = Arc::new(43u64);
        {
            let mut writer = tracked.write().await;
            writer.publish(Arc::clone(&replacement));
        }
        assert!(Arc::ptr_eq(
            &managed.resolve_snapshot(unexpected_init).await,
            &replacement
        ));
        {
            let mut writer = tracked.write().await;
            writer.publish(Arc::clone(&expected));
        }
        assert!(Arc::ptr_eq(
            &immutable.resolve_snapshot(unexpected_init).await,
            &expected
        ));
        assert!(Arc::ptr_eq(
            &managed.resolve_snapshot(unexpected_init).await,
            &expected
        ));
    });

    let mut group = criterion.benchmark_group("provider_resolve");
    group.bench_function("immutable_warm", |b| {
        b.to_async(&runtime).iter(|| async {
            drop(black_box(
                black_box(&immutable)
                    .resolve_snapshot(unexpected_init)
                    .await,
            ));
        });
    });
    group.bench_function("managed_warm", |b| {
        b.to_async(&runtime).iter(|| async {
            drop(black_box(
                black_box(&managed).resolve_snapshot(unexpected_init).await,
            ));
        });
    });
    group.bench_function("arc_clone_drop_reference", |b| {
        b.to_async(&runtime).iter(|| async {
            drop(black_box(Arc::clone(black_box(&expected))));
        });
    });
    group.finish();
}
