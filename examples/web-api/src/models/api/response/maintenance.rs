use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MaintenanceOutcome {
    pub archived: usize,
    pub refreshed: usize,
    pub runs: u64,
}
