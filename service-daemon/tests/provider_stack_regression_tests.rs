use service_daemon::{Registry, ServiceDaemon, provider, service};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::timeout;

static INIT_ORDER: AtomicUsize = AtomicUsize::new(0);
static SERVICE_OBSERVED: AtomicUsize = AtomicUsize::new(0);

fn mark_provider(index: usize) {
    let previous =
        INIT_ORDER.compare_exchange(index, index + 1, Ordering::SeqCst, Ordering::SeqCst);
    assert_eq!(
        previous,
        Ok(index),
        "provider P{index:02} started before its dependency was initialized"
    );
}

#[derive(Clone)]
struct P00;
#[provider]
async fn p00() -> P00 {
    mark_provider(0);
    P00
}

#[derive(Clone)]
struct P01;
#[provider]
async fn p01(_p: Arc<P00>) -> P01 {
    mark_provider(1);
    P01
}

#[derive(Clone)]
struct P02;
#[provider]
async fn p02(_p: Arc<P01>) -> P02 {
    mark_provider(2);
    P02
}

#[derive(Clone)]
struct P03;
#[provider]
async fn p03(_p: Arc<P02>) -> P03 {
    mark_provider(3);
    P03
}

#[derive(Clone)]
struct P04;
#[provider]
async fn p04(_p: Arc<P03>) -> P04 {
    mark_provider(4);
    P04
}

#[derive(Clone)]
struct P05;
#[provider]
async fn p05(_p: Arc<P04>) -> P05 {
    mark_provider(5);
    P05
}

#[derive(Clone)]
struct P06;
#[provider]
async fn p06(_p: Arc<P05>) -> P06 {
    mark_provider(6);
    P06
}

#[derive(Clone)]
struct P07;
#[provider]
async fn p07(_p: Arc<P06>) -> P07 {
    mark_provider(7);
    P07
}

#[derive(Clone)]
struct P08;
#[provider]
async fn p08(_p: Arc<P07>) -> P08 {
    mark_provider(8);
    P08
}

#[derive(Clone)]
struct P09;
#[provider]
async fn p09(_p: Arc<P08>) -> P09 {
    mark_provider(9);
    P09
}

#[derive(Clone)]
struct P10;
#[provider]
async fn p10(_p: Arc<P09>) -> P10 {
    mark_provider(10);
    P10
}

#[derive(Clone)]
struct P11;
#[provider]
async fn p11(_p: Arc<P10>) -> P11 {
    mark_provider(11);
    P11
}

#[derive(Clone)]
struct P12;
#[provider]
async fn p12(_p: Arc<P11>) -> P12 {
    mark_provider(12);
    P12
}

#[derive(Clone)]
struct P13;
#[provider]
async fn p13(_p: Arc<P12>) -> P13 {
    mark_provider(13);
    P13
}

#[derive(Clone)]
struct P14;
#[provider]
async fn p14(_p: Arc<P13>) -> P14 {
    mark_provider(14);
    P14
}

#[derive(Clone)]
struct P15;
#[provider]
async fn p15(_p: Arc<P14>) -> P15 {
    mark_provider(15);
    P15
}

#[derive(Clone)]
struct P16;
#[provider]
async fn p16(_p: Arc<P15>) -> P16 {
    mark_provider(16);
    P16
}

#[derive(Clone)]
struct P17;
#[provider]
async fn p17(_p: Arc<P16>) -> P17 {
    mark_provider(17);
    P17
}

#[derive(Clone)]
struct P18;
#[provider]
async fn p18(_p: Arc<P17>) -> P18 {
    mark_provider(18);
    P18
}

#[derive(Clone)]
struct P19;
#[provider]
async fn p19(_p: Arc<P18>) -> P19 {
    mark_provider(19);
    P19
}

#[derive(Clone)]
struct P20;
#[provider]
async fn p20(_p: Arc<P19>) -> P20 {
    mark_provider(20);
    P20
}

#[derive(Clone)]
struct BranchLeft;
#[provider]
async fn branch_left(_p: Arc<P20>) -> BranchLeft {
    BranchLeft
}

#[derive(Clone)]
struct BranchRight;
#[provider]
async fn branch_right(_p: Arc<P20>) -> BranchRight {
    BranchRight
}

#[derive(Clone)]
struct Join;
#[provider]
async fn join(_left: Arc<BranchLeft>, _right: Arc<BranchRight>) -> Join {
    Join
}

#[service(tags = ["provider_stack_regression"])]
async fn provider_stack_regression_service(_join: Arc<Join>) -> anyhow::Result<()> {
    SERVICE_OBSERVED.fetch_add(1, Ordering::SeqCst);
    service_daemon::done();

    while !service_daemon::is_shutdown() {
        service_daemon::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[tokio::test]
async fn deep_provider_graph_is_prepared_before_service_body_runs() {
    INIT_ORDER.store(0, Ordering::SeqCst);
    SERVICE_OBSERVED.store(0, Ordering::SeqCst);

    let daemon = ServiceDaemon::builder()
        .with_registry(
            Registry::builder()
                .with_tag("provider_stack_regression")
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;

    timeout(Duration::from_secs(5), async {
        while SERVICE_OBSERVED.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("service should observe prepared provider graph before timeout");

    assert_eq!(INIT_ORDER.load(Ordering::SeqCst), 21);

    cancel.cancel();
    timeout(Duration::from_secs(5), daemon.wait())
        .await
        .expect("daemon wait should not time out")
        .expect("daemon wait should succeed");
}
