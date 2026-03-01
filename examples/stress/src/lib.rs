// Half-batch 0h: first 50 services (services 0-49), included when s50+
#[cfg(feature = "s50")]
pub mod batch_0h;

// Batch 0: full first 100 (services 0-99), remaining 50 (50-99) gated on s100+
#[cfg(feature = "s100")]
pub mod batch_0;

// Half-batch 1h: services 100-149, included when s150+
#[cfg(feature = "s150")]
pub mod batch_1h;

// Batch 1: included when s200+ (services 100-199, remaining 150-199)
#[cfg(feature = "s200")]
pub mod batch_1;

// Batch 2: included when s300+ (services 200-299)
#[cfg(feature = "s300")]
pub mod batch_2;

// Batch 3: included when s400+ (services 300-399)
#[cfg(feature = "s400")]
pub mod batch_3;

// Batch 4: included when s500+ (services 400-499)
#[cfg(feature = "s500")]
pub mod batch_4;

// Batch 5: included when s600+ (services 500-599)
#[cfg(feature = "s600")]
pub mod batch_5;

// Batch 6: included when s700+ (services 600-699)
#[cfg(feature = "s700")]
pub mod batch_6;

// Batch 7: included when s800+ (services 700-799)
#[cfg(feature = "s800")]
pub mod batch_7;

// Batch 8: included when s900+ (services 800-899)
#[cfg(feature = "s900")]
pub mod batch_8;

// Batch 9: included when s1000 (services 900-999)
#[cfg(feature = "s1000")]
pub mod batch_9;
