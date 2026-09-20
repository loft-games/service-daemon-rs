use std::{hint::black_box, sync::Arc, time::Instant};

use criterion::Criterion;

use crate::ProviderInitError;
use crate::core::context::{__run_daemon_resources_scope, DaemonResources};
use crate::core::provider_scope::{resolve_provider_rwlock, resolve_provider_snapshot};

fn unexpected_init() -> std::future::Ready<Result<Arc<u64>, ProviderInitError>> {
    panic!("a warm provider must not invoke its initializer")
}

pub(super) fn run(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let expected = Arc::new(42u64);
    let immutable = DaemonResources::new();
    let managed = DaemonResources::new();

    runtime.block_on(async {
        for resources in [&immutable, &managed] {
            __run_daemon_resources_scope(resources.clone(), || async {
                let initial = resolve_provider_snapshot(|| async { Ok(expected.clone()) })
                    .await
                    .unwrap();
                assert!(Arc::ptr_eq(&initial, &expected));
            })
            .await;
        }
        __run_daemon_resources_scope(managed.clone(), || async {
            let tracked = resolve_provider_rwlock(unexpected_init).await.unwrap();
            let replacement = Arc::new(43u64);
            tracked.write().await.publish(replacement.clone());
            assert!(Arc::ptr_eq(
                &resolve_provider_snapshot(unexpected_init).await.unwrap(),
                &replacement
            ));
            tracked.write().await.publish(expected.clone());
        })
        .await;
    });

    let mut group = criterion.benchmark_group("provider_resolve");
    for (name, resources) in [("immutable_warm", immutable), ("managed_warm", managed)] {
        group.bench_function(name, |bench| {
            bench.iter_custom(|iterations| {
                runtime.block_on(__run_daemon_resources_scope(resources.clone(), || async {
                    let start = Instant::now();
                    for _ in 0..iterations {
                        drop(black_box(
                            resolve_provider_snapshot(unexpected_init).await.unwrap(),
                        ));
                    }
                    start.elapsed()
                }))
            });
        });
    }
    group.bench_function("arc_clone_drop_reference", |bench| {
        bench.to_async(&runtime).iter(|| async {
            drop(black_box(Arc::clone(black_box(&expected))));
        });
    });
    group.finish();
}
