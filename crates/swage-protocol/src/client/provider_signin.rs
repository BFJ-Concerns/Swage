//! Interactive provider sign-in types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignInMethod {
    #[default]
    Browser,
    DeviceCode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
pub struct StartProviderSignInRequest {
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub method: SignInMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum StartProviderSignInResponse {
    Browser {
        auth_url: String,
        login_id: String,
    },
    DeviceCode {
        verification_url: String,
        user_code: String,
        login_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSignInStatus {
    SignedOut,
    SigningIn,
    SignedIn,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProviderAccount {
    pub email: Option<String>,
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProviderSignInState {
    pub status: ProviderSignInStatus,
    pub account: Option<ProviderAccount>,
}
