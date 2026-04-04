//! Stress half-batch 1h: services 1050..1099

use std::time::Duration;

use service_daemon::service;

macro_rules! define_stress_service {
    ($id:ident) => {
        #[service]
        #[allow(sync_handler)]
        pub async fn $id() -> anyhow::Result<()> {
            while !service_daemon::is_shutdown() {
                if !service_daemon::sleep(Duration::from_secs(3600)).await {
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
    1050, 1051, 1052, 1053, 1054, 1055, 1056, 1057, 1058, 1059, 1060, 1061, 1062, 1063, 1064, 1065,
    1066, 1067, 1068, 1069, 1070, 1071, 1072, 1073, 1074, 1075, 1076, 1077, 1078, 1079, 1080, 1081,
    1082, 1083, 1084, 1085, 1086, 1087, 1088, 1089, 1090, 1091, 1092, 1093, 1094, 1095, 1096, 1097,
    1098, 1099,
);
