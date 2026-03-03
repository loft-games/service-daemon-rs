//! Stress batch 5: services 500..599

use service_daemon::service;

macro_rules! define_stress_service {
    ($id:ident) => {
        #[service]
        #[allow(sync_handler)]
        pub async fn $id() -> anyhow::Result<()> {
            while !service_daemon::is_shutdown() {
                if !service_daemon::sleep(std::time::Duration::from_secs(3600)).await {
                    break;
                }
            }
            Ok(())
        }
    };
}

macro_rules! define_batch {
    ($($n:literal),* $(,)?) => {
        paste::paste! {
            $(
                define_stress_service!([<stress_svc_ $n>]);
            )*
        }
    };
}

define_batch!(
    500, 501, 502, 503, 504, 505, 506, 507, 508, 509, 510, 511, 512, 513, 514, 515, 516, 517, 518,
    519, 520, 521, 522, 523, 524, 525, 526, 527, 528, 529, 530, 531, 532, 533, 534, 535, 536, 537,
    538, 539, 540, 541, 542, 543, 544, 545, 546, 547, 548, 549, 550, 551, 552, 553, 554, 555, 556,
    557, 558, 559, 560, 561, 562, 563, 564, 565, 566, 567, 568, 569, 570, 571, 572, 573, 574, 575,
    576, 577, 578, 579, 580, 581, 582, 583, 584, 585, 586, 587, 588, 589, 590, 591, 592, 593, 594,
    595, 596, 597, 598, 599,
);
