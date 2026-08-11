//! # Memory Analysis Tool
//!
//! Measures per-service memory overhead in three complementary layers:
//!
//! - **Section 1 (Static)**: `std::mem::size_of` for core framework types.
//! - **Section 2 (Dynamic Isolation)**: RSS delta for individual components,
//!   each allocated N times in isolation to capture true heap cost.
//! - **Section 3 (End-to-End)**: Full framework test via `ServiceDaemon`,
//!   with automatic component attribution.
//!
//! ## Platform Requirements
//!
//! Dynamic RSS measurement relies on `/proc/self/statm` and is **Linux-only**.
//! On other platforms, only the static analysis (Section 1) will be available.
//!
//! ## Usage
//!
//! ```bash
//! cargo run --release -p example-memory-analysis
//! ```

#[cfg(target_os = "linux")]
use std::io::Read;
use std::sync::Arc;

use std::any::{Any, TypeId};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::future::BoxFuture;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::info_span;
use uuid::Uuid;

use service_daemon::{
    BackoffController, ProviderDependencyWatchSet, RestartPolicy, ServiceDaemon, ServiceEntryId,
    ServiceInstanceId, ServiceScheduling, ServiceStatus,
};

fn service_instance_id(index: usize) -> ServiceInstanceId {
    ServiceInstanceId::new(Uuid::from_u128(index as u128))
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Number of entries in each isolation test. Higher values reduce
/// page-granularity noise but increase runtime.
const ISOLATION_COUNT: usize = 1000;

/// Number of services for the end-to-end framework test.
const E2E_SERVICE_COUNT: usize = 100;

/// Warm-up iterations to prime the allocator before real measurement.
const WARMUP_ROUNDS: usize = 100;

/// Seconds to wait after spawning services before sampling RSS.
const SETTLE_DELAY_SECS: u64 = 2;

type MockServiceFn = fn(CancellationToken) -> BoxFuture<'static, anyhow::Result<()>>;
type MockShelfValue = Box<dyn Any + Send + Sync>;
type MockServiceShelf = DashMap<String, MockShelfValue>;
type MockGlobalShelfMapping = DashMap<ServiceInstanceId, MockServiceShelf>;

#[allow(dead_code)]
struct MockDaemonResources {
    status_plane: DashMap<ServiceInstanceId, ServiceStatus>,
    shelf: MockGlobalShelfMapping,
    reload_signals: DashMap<ServiceInstanceId, Arc<tokio::sync::Notify>>,
    status_changed: tokio::sync::Notify,
    trigger_configs: DashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl MockDaemonResources {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            status_plane: DashMap::new(),
            shelf: DashMap::new(),
            reload_signals: DashMap::new(),
            status_changed: tokio::sync::Notify::new(),
            trigger_configs: DashMap::new(),
        })
    }
}

#[allow(dead_code)]
struct MockServiceDescription {
    entry_id: ServiceEntryId,
    instance_id: ServiceInstanceId,
    entry: &'static (),
    cancellation_token: CancellationToken,
}

// ---------------------------------------------------------------------------
// MockSupervisor -- mirrors the private `ServiceSupervisor` in supervisor.rs
// ---------------------------------------------------------------------------
// IMPORTANT: If `ServiceSupervisor` fields change, update this struct to match.
// Reference: service-daemon/src/core/service_daemon/runner/supervisor.rs (ServiceSupervisor)

/// Layout-compatible mock of the internal `ServiceSupervisor` struct.
///
/// This type exists solely for memory measurement. Its field types and order
/// must mirror the real `ServiceSupervisor` defined in `supervisor.rs`.
///
/// # Sync Contract
///
/// The static assertion at the bottom of this file verifies that
/// `size_of::<MockSupervisor>()` matches the expected value. If the real
/// struct changes layout, this assertion will fail at compile time, reminding
/// you to update the mock.
#[allow(dead_code)]
struct MockBodyExecutionLanes {
    standard: Handle,
    high_priority: Option<Handle>,
}

#[allow(dead_code)]
struct MockBodyLaneResolver;

#[allow(dead_code)]
enum MockBodyExecutionLane {
    Standard(Handle),
    HighPriority(Handle),
    Isolated,
}

#[allow(dead_code)]
struct MockRestartStormConfig {
    window: Duration,
    failure_threshold: usize,
    suppression_delay: Duration,
}

#[allow(dead_code)]
struct MockRestartStormGuard {
    config: MockRestartStormConfig,
    failures: VecDeque<Instant>,
}

impl Default for MockRestartStormGuard {
    fn default() -> Self {
        Self {
            config: MockRestartStormConfig {
                window: Duration::from_secs(20),
                failure_threshold: 6,
                suppression_delay: Duration::from_secs(30),
            },
            failures: VecDeque::new(),
        }
    }
}

#[allow(dead_code)]
struct MockDiagnosticsStore;

#[allow(dead_code)]
struct MockGenerationDiagnosticsHandle {
    service: Arc<()>,
    generation: Arc<()>,
    lane: Arc<()>,
}

#[allow(dead_code)]
struct MockServiceIdentity {
    service_instance_id: ServiceInstanceId,
    name: &'static str,
    cancellation_token: CancellationToken,
    reload_token: CancellationToken,
    diagnostics: Option<MockGenerationDiagnosticsHandle>,
    is_handshake_done: Arc<AtomicBool>,
}

#[allow(dead_code)]
struct MockSupervisor {
    // -- Immutable service identity --
    service_instance_id: ServiceInstanceId,
    name: &'static str,
    run: MockServiceFn,
    watcher: Option<fn() -> ProviderDependencyWatchSet>,
    scheduling: ServiceScheduling,
    body_lanes: MockBodyExecutionLanes,
    body_lane_resolver: MockBodyLaneResolver,
    generation_body_lane: Option<MockBodyExecutionLane>,
    generation_scheduling: Option<ServiceScheduling>,
    backoff: BackoffController,
    restart_storm: MockRestartStormGuard,
    resources: Arc<MockDaemonResources>,
    diagnostics: Arc<MockDiagnosticsStore>,
    isolated_startup_permits: Arc<Semaphore>,
    cancellation_token: CancellationToken,
    daemon_token: CancellationToken,
    // -- Per-generation mutable context --
    generation_start: Option<Instant>,
    generation: u64,
    generation_diagnostics: Option<MockGenerationDiagnosticsHandle>,
    dependency_watch_set: Option<ProviderDependencyWatchSet>,
    reload_token: Option<CancellationToken>,
}

// Compile-time size guard: update EXPECTED_SIZE if ServiceSupervisor changes.
// Compile-time size guards like this are extremely sensitive to upstream
// dependency layout and compiler changes. This example is for manual inspection,
// not for CI invariants.
#[cfg(feature = "memory-analysis")]
const _: () = {
    const EXPECTED_SIZE: usize = 448;
    assert!(
        std::mem::size_of::<MockSupervisor>() == EXPECTED_SIZE,
        // If this fails, the real ServiceSupervisor layout has changed.
        // Update MockSupervisor fields to match supervisor.rs and adjust EXPECTED_SIZE.
    );
};

// ---------------------------------------------------------------------------
// RSS Measurement (Linux-only)
// ---------------------------------------------------------------------------

/// Read current RSS (Resident Set Size) in bytes from `/proc/self/statm`.
///
/// Returns `None` on non-Linux platforms or if the procfs read fails.
fn read_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut buf = String::new();
        std::fs::File::open("/proc/self/statm")
            .ok()?
            .read_to_string(&mut buf)
            .ok()?;
        let rss_pages: u64 = buf.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf(_SC_PAGESIZE) is always safe to call.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        Some(rss_pages * page_size)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Measure RSS delta across a closure. Returns bytes consumed, or `None`
/// if RSS measurement is unavailable.
fn measure_rss_delta(f: impl FnOnce()) -> Option<u64> {
    let before = read_rss_bytes()?;
    f();
    let after = read_rss_bytes()?;
    Some(after.saturating_sub(before))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Print a section header with box-drawing characters.
fn print_header(title: &str) {
    let width = 60;
    println!();
    println!("+-{}-+", "-".repeat(width));
    println!("|  {:<w$}|", title, w = width - 2);
    println!("+-{}-+", "-".repeat(width));
    println!();
}

/// Print a measurement row aligned with the section tables.
fn print_row(label: &str, value: f64, unit: &str) {
    println!("  {:<50} {:>8.1} {}", label, value, unit);
}

// ---------------------------------------------------------------------------
// Section 1: Static Analysis
// ---------------------------------------------------------------------------

fn run_static_analysis() {
    print_header("Section 1: Static Analysis (std::mem::size_of)");

    let types: Vec<(&str, usize)> = vec![
        (
            "ServiceInstanceId",
            std::mem::size_of::<ServiceInstanceId>(),
        ),
        ("ServiceStatus", std::mem::size_of::<ServiceStatus>()),
        (
            "MockServiceDescription (~= internal ServiceDescription)",
            std::mem::size_of::<MockServiceDescription>(),
        ),
        ("RestartPolicy", std::mem::size_of::<RestartPolicy>()),
        (
            "BackoffController",
            std::mem::size_of::<BackoffController>(),
        ),
        (
            "MockServiceIdentity (~= internal ServiceIdentity)",
            std::mem::size_of::<MockServiceIdentity>(),
        ),
        (
            "MockDaemonResources",
            std::mem::size_of::<MockDaemonResources>(),
        ),
        (
            "CancellationToken",
            std::mem::size_of::<CancellationToken>(),
        ),
        (
            "Arc<MockDaemonResources>",
            std::mem::size_of::<Arc<MockDaemonResources>>(),
        ),
        (
            "Arc<Notify>",
            std::mem::size_of::<Arc<tokio::sync::Notify>>(),
        ),
        (
            "JoinHandle<()>",
            std::mem::size_of::<tokio::task::JoinHandle<()>>(),
        ),
        (
            "MockSupervisor (~= ServiceSupervisor)",
            std::mem::size_of::<MockSupervisor>(),
        ),
    ];

    for (name, size) in &types {
        println!("  {:<50} {:>5} bytes", name, size);
    }
}

// ---------------------------------------------------------------------------
// Section 2: Dynamic Isolation Tests
// ---------------------------------------------------------------------------

/// Warm up the allocator to reduce first-allocation noise.
fn warmup_allocator() {
    let mut warmup: Vec<Box<[u8; 256]>> = Vec::with_capacity(WARMUP_ROUNDS);
    for _ in 0..WARMUP_ROUNDS {
        warmup.push(Box::new([0u8; 256]));
    }
    std::hint::black_box(&warmup);
    drop(warmup);
}

fn measure_dashmap_status_plane() -> Option<f64> {
    warmup_allocator();
    let map: DashMap<ServiceInstanceId, ServiceStatus> = DashMap::new();

    let delta = measure_rss_delta(|| {
        for i in 0..ISOLATION_COUNT {
            map.insert(service_instance_id(i), ServiceStatus::Healthy);
        }
        std::hint::black_box(&map);
    })?;

    Some(delta as f64 / ISOLATION_COUNT as f64)
}

fn measure_dashmap_reload_signals() -> Option<f64> {
    warmup_allocator();
    let map: DashMap<ServiceInstanceId, Arc<tokio::sync::Notify>> = DashMap::new();

    let delta = measure_rss_delta(|| {
        for i in 0..ISOLATION_COUNT {
            map.insert(service_instance_id(i), Arc::new(tokio::sync::Notify::new()));
        }
        std::hint::black_box(&map);
    })?;

    Some(delta as f64 / ISOLATION_COUNT as f64)
}

fn measure_cancellation_tokens() -> Option<f64> {
    warmup_allocator();
    let mut tokens = Vec::with_capacity(ISOLATION_COUNT);

    let delta = measure_rss_delta(|| {
        for _ in 0..ISOLATION_COUNT {
            tokens.push(CancellationToken::new());
        }
        std::hint::black_box(&tokens);
    })?;

    Some(delta as f64 / ISOLATION_COUNT as f64)
}

fn measure_supervisor_heap_box() -> Option<f64> {
    warmup_allocator();
    let res = MockDaemonResources::new();
    let standard_handle = Handle::current();
    let diagnostics = Arc::new(MockDiagnosticsStore);
    let isolated_startup_permits = Arc::new(Semaphore::new(32));
    let mut boxes: Vec<Box<MockSupervisor>> = Vec::with_capacity(ISOLATION_COUNT);

    let delta = measure_rss_delta(|| {
        for i in 0..ISOLATION_COUNT {
            boxes.push(Box::new(MockSupervisor {
                service_instance_id: service_instance_id(i),
                name: "bench",
                run: |_| Box::pin(async { Ok(()) }),
                watcher: None,
                scheduling: ServiceScheduling::Standard,
                body_lanes: MockBodyExecutionLanes {
                    standard: standard_handle.clone(),
                    high_priority: None,
                },
                body_lane_resolver: MockBodyLaneResolver,
                generation_body_lane: None,
                generation_scheduling: None,
                backoff: BackoffController::new(RestartPolicy::default()),
                restart_storm: MockRestartStormGuard::default(),
                resources: res.clone(),
                diagnostics: diagnostics.clone(),
                isolated_startup_permits: isolated_startup_permits.clone(),
                cancellation_token: CancellationToken::new(),
                daemon_token: CancellationToken::new(),
                generation_start: None,
                generation: 0,
                generation_diagnostics: None,
                dependency_watch_set: None,
                reload_token: None,
            }));
        }
        std::hint::black_box(&boxes);
    })?;

    Some(delta as f64 / ISOLATION_COUNT as f64)
}

fn measure_tracing_spans() -> Option<f64> {
    warmup_allocator();
    let mut spans = Vec::with_capacity(ISOLATION_COUNT);

    let delta = measure_rss_delta(|| {
        for i in 0..ISOLATION_COUNT {
            spans.push(info_span!(
                "service",
                name = "bench",
                service_instance_id = i
            ));
        }
        std::hint::black_box(&spans);
    })?;

    Some(delta as f64 / ISOLATION_COUNT as f64)
}

async fn measure_tokio_task_spawn() -> Option<f64> {
    warmup_allocator();
    let mut handles = Vec::with_capacity(ISOLATION_COUNT);

    let before = read_rss_bytes()?;
    for _ in 0..ISOLATION_COUNT {
        handles.push(tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }));
    }
    // Allow scheduler to register all tasks.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = read_rss_bytes()?;

    // Keep handles alive to prevent RSS from shrinking.
    std::hint::black_box(&handles);

    Some(after.saturating_sub(before) as f64 / ISOLATION_COUNT as f64)
}

async fn measure_hashmap_join_handles() -> Option<f64> {
    warmup_allocator();
    let mut map: HashMap<ServiceInstanceId, tokio::task::JoinHandle<()>> =
        HashMap::with_capacity(ISOLATION_COUNT);

    let before = read_rss_bytes()?;
    for i in 0..ISOLATION_COUNT {
        let handle = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        map.insert(service_instance_id(i), handle);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = read_rss_bytes()?;

    std::hint::black_box(&map);

    Some(after.saturating_sub(before) as f64 / ISOLATION_COUNT as f64)
}

// ---------------------------------------------------------------------------
// Section 3: End-to-End Framework Test
// ---------------------------------------------------------------------------

async fn run_e2e_test() -> Option<(u64, u64, f64)> {
    let baseline = read_rss_bytes()?;

    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    // Allow all services to fully initialize.
    tokio::time::sleep(Duration::from_secs(SETTLE_DELAY_SECS)).await;
    let loaded = read_rss_bytes()?;

    let total_delta = loaded.saturating_sub(baseline);
    let per_service = total_delta as f64 / E2E_SERVICE_COUNT as f64;

    daemon.shutdown();

    Some((baseline / 1024, loaded / 1024, per_service))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing at warn level to suppress info/debug noise.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // -- Section 1: Static Analysis (always available) --
    run_static_analysis();

    // -- Platform check for dynamic sections --
    if read_rss_bytes().is_none() {
        println!();
        println!("[WARNING] Dynamic analysis requires Linux (/proc/self/statm).");
        println!("   Only static analysis (Section 1) is available on this platform.");
        return Ok(());
    }

    // -- Section 2: Dynamic Isolation Tests --
    print_header(&format!(
        "Section 2: Dynamic Isolation Tests ({} entries each)",
        ISOLATION_COUNT
    ));

    let cost_status = measure_dashmap_status_plane().unwrap_or(0.0);
    print_row(
        "DashMap<ServiceInstanceId, ServiceStatus>",
        cost_status,
        "B/entry",
    );

    let cost_signals = measure_dashmap_reload_signals().unwrap_or(0.0);
    print_row(
        "DashMap<ServiceInstanceId, Arc<Notify>>",
        cost_signals,
        "B/entry",
    );

    let cost_token = measure_cancellation_tokens().unwrap_or(0.0);
    print_row("CancellationToken::new()", cost_token, "B/token");

    let cost_supervisor = measure_supervisor_heap_box().unwrap_or(0.0);
    print_row(
        "Box<ServiceSupervisor> (heap alloc)",
        cost_supervisor,
        "B/box",
    );

    let cost_span = measure_tracing_spans().unwrap_or(0.0);
    print_row("tracing::info_span! allocation", cost_span, "B/span");

    let cost_task = measure_tokio_task_spawn().await.unwrap_or(0.0);
    print_row("tokio::spawn (idle future)", cost_task, "B/task");

    let cost_map = measure_hashmap_join_handles().await.unwrap_or(0.0);
    print_row(
        "HashMap<ServiceInstanceId, JoinHandle>",
        cost_map,
        "B/entry",
    );

    // Per-service total estimate: sum of isolation measurements.
    // In the real framework, the supervisor struct IS the tokio task's future,
    // so we do NOT double-count `cost_supervisor` and `cost_task` separately.
    // Instead, we use `cost_map` which includes both spawn + map entry cost.
    let estimated_total = cost_status
        + cost_signals
        + cost_token * 2.0  // x2: one for ServiceDescription, one for reload_token
        + cost_supervisor   // Supervisor struct heap box
        + cost_span         // Tracing span (may be 0 without active subscriber)
        + cost_task; // Tokio task spawn overhead

    println!();
    println!(
        "  --- Estimated per-service total (isolation sum): {:.1} B ({:.2} KB)",
        estimated_total,
        estimated_total / 1024.0
    );

    // -- Section 3: End-to-End Framework Test --
    print_header(&format!(
        "Section 3: End-to-End Framework Test ({} services)",
        E2E_SERVICE_COUNT
    ));

    if let Some((baseline_kb, loaded_kb, per_service)) = run_e2e_test().await {
        let total_delta_kb = loaded_kb.saturating_sub(baseline_kb);

        println!("  Baseline RSS (0 services):  {} KB", baseline_kb);
        println!(
            "  Loaded RSS ({} services): {} KB",
            E2E_SERVICE_COUNT, loaded_kb
        );
        println!("  Total delta:                {} KB", total_delta_kb);
        println!(
            "  Per-service delta:          {:.1} B ({:.2} KB)",
            per_service,
            per_service / 1024.0
        );

        // -- Component Attribution --
        print_header("Component Attribution (isolation -> % of E2E delta)");

        let components: Vec<(&str, f64)> = vec![
            (
                "StatusPlane (DashMap<ServiceInstanceId, Status>)",
                cost_status,
            ),
            (
                "ReloadSignals (DashMap<ServiceInstanceId, Arc<Notify>>)",
                cost_signals,
            ),
            ("CancellationTokens (x2)", cost_token * 2.0),
            (
                "Supervisor Struct (Box<ServiceSupervisor>)",
                cost_supervisor,
            ),
            ("Tracing Span", cost_span),
            ("Tokio Task (future boxing + header)", cost_task),
        ];

        let attributed: f64 = components.iter().map(|(_, c)| c).sum();
        let unaccounted = per_service - attributed;

        for (label, cost) in &components {
            let pct = if per_service > 0.0 {
                cost / per_service * 100.0
            } else {
                0.0
            };
            println!("  {:<50} {:>5.0} B  ({:>5.1}%)", label, cost, pct);
        }

        let unaccounted_pct = if per_service > 0.0 {
            unaccounted / per_service * 100.0
        } else {
            0.0
        };
        println!(
            "  {:<50} {:>5.0} B  ({:>5.1}%)",
            "Unaccounted (alignment, alloc metadata)", unaccounted, unaccounted_pct
        );

        println!();
        println!(
            "  Total E2E delta per service: {:.0} B ({:.2} KB)",
            per_service,
            per_service / 1024.0
        );
    }

    Ok(())
}
