use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ItemResponse {
    pub id: u64,
    pub label: String,
    pub stale: bool,
    pub archived: bool,
    pub revision: u64,
}
