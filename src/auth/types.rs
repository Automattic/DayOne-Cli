use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginV3RequestBody {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginV3Response {
    pub token: String,
    pub created_at: String,
    pub user: Value,
    #[serde(default, alias = "masterKey")]
    pub master_key: Option<String>,
    #[serde(default, alias = "e2eeDisplayKey")]
    pub e2ee_display_key: Option<String>,
}
