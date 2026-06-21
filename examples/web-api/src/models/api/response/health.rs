use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HealthResponse {
    pub service_name: String,
    pub item_count: usize,
    pub maintenance_runs: u64,
}
