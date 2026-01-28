use service_daemon::{Provided, RestartPolicy, ServiceDaemon, trigger};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

// --- Test Setup ---

#[derive(Clone, Debug, PartialEq)]
pub struct MyPayload(pub String);

pub struct Queue1;
pub struct Queue2;

static TX1: tokio::sync::OnceCell<mpsc::Sender<MyPayload>> = tokio::sync::OnceCell::const_new();
static RX1: tokio::sync::OnceCell<Arc<Mutex<mpsc::Receiver<MyPayload>>>> =
    tokio::sync::OnceCell::const_new();
static TX2: tokio::sync::OnceCell<mpsc::Sender<MyPayload>> = tokio::sync::OnceCell::const_new();
static RX2: tokio::sync::OnceCell<Arc<Mutex<mpsc::Receiver<MyPayload>>>> =
    tokio::sync::OnceCell::const_new();

impl Provided for Queue1 {
    async fn resolve() -> Arc<Self> {
        Arc::new(Self)
    }
}
impl Provided for Queue2 {
    async fn resolve() -> Arc<Self> {
        Arc::new(Self)
    }
}

pub struct LB1 {
    pub rx: Arc<Mutex<mpsc::Receiver<MyPayload>>>,
}
impl Provided for LB1 {
    async fn resolve() -> Arc<Self> {
        Arc::new(Self {
            rx: RX1.get().unwrap().clone(),
        })
    }
}

pub struct LB2 {
    pub rx: Arc<Mutex<mpsc::Receiver<MyPayload>>>,
}
impl Provided for LB2 {
    async fn resolve() -> Arc<Self> {
        Arc::new(Self {
            rx: RX2.get().unwrap().clone(),
        })
    }
}

pub struct Counter(pub Arc<Mutex<u32>>);
static SHARED_COUNTER: tokio::sync::OnceCell<Arc<Counter>> = tokio::sync::OnceCell::const_new();

impl Provided for Counter {
    async fn resolve() -> Arc<Self> {
        SHARED_COUNTER
            .get_or_init(|| async { Arc::new(Self(Arc::new(Mutex::new(0)))) })
            .await
            .clone()
    }
}

// --- Triggers ---

#[trigger(template = LBQueue, target = LB1)]
pub async fn payload_only(payload: MyPayload, counter: Arc<Counter>) -> anyhow::Result<()> {
    let mut count = counter.0.lock().await;
    *count += 1;
    println!("Trigger 'payload_only' fired with: {:?}", payload);
    assert_eq!(payload.0, "hello");
    Ok(())
}

pub struct TestCron;
impl Provided for TestCron {
    async fn resolve() -> Arc<Self> {
        Arc::new(Self)
    }
}
impl TestCron {
    pub fn as_str(&self) -> &str {
        "*/1 * * * * *"
    }
}

#[trigger(template = Cron, target = TestCron)]
pub async fn di_only(counter: Arc<Counter>) -> anyhow::Result<()> {
    let mut count = counter.0.lock().await;
    *count += 1;
    println!("Trigger 'di_only' fired");
    Ok(())
}

#[trigger(template = LBQueue, target = LB2)]
pub async fn arc_payload(
    #[payload] payload: Arc<MyPayload>,
    counter: Arc<Counter>,
) -> anyhow::Result<()> {
    let mut count = counter.0.lock().await;
    *count += 1;
    println!("Trigger 'arc_payload' fired with: {:?}", payload);
    assert_eq!(payload.0, "arc_hello");
    Ok(())
}

#[tokio::test]
async fn test_declarative_trigger_patterns() {
    // Init queues
    let (tx1, rx1) = mpsc::channel(32);
    let _ = TX1.set(tx1);
    let _ = RX1.set(Arc::new(Mutex::new(rx1)));
    let (tx2, rx2) = mpsc::channel(32);
    let _ = TX2.set(tx2);
    let _ = RX2.set(Arc::new(Mutex::new(rx2)));

    let daemon = ServiceDaemon::from_registry_with_policy(RestartPolicy::for_testing());

    println!(
        "Registry contains {} entries:",
        service_daemon::SERVICE_REGISTRY.len()
    );
    for entry in service_daemon::SERVICE_REGISTRY.iter() {
        println!("  - {} ({})", entry.name, entry.module);
    }

    let counter = Counter::resolve().await;

    // Send items
    TX1.get()
        .unwrap()
        .send(MyPayload("hello".to_string()))
        .await
        .unwrap();
    TX2.get()
        .unwrap()
        .send(MyPayload("arc_hello".to_string()))
        .await
        .unwrap();

    // Run daemon
    daemon
        .run_for_duration(std::time::Duration::from_secs(3))
        .await
        .unwrap();

    // Check counter
    let final_count = *counter.0.lock().await;
    println!("Final count: {}", final_count);
    assert!(
        final_count >= 3,
        "Expected at least 3 trigger executions (LB1, LB2, Cron), got {}",
        final_count
    );
}
