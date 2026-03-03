//! Stress half-batch 0h: services 1000..1049

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
    1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011, 1012, 1013, 1014, 1015,
    1016, 1017, 1018, 1019, 1020, 1021, 1022, 1023, 1024, 1025, 1026, 1027, 1028, 1029, 1030, 1031,
    1032, 1033, 1034, 1035, 1036, 1037, 1038, 1039, 1040, 1041, 1042, 1043, 1044, 1045, 1046, 1047,
    1048, 1049,
);
