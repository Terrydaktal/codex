use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::auth::AgentIdentityStorage;
use codex_login::auth::read_codex_api_key_from_env;
use codex_login::load_auth_dot_json;
use codex_login::read_codex_access_token_from_env;
use codex_login::read_openai_api_key_from_env;
use codex_login::token_data::parse_chatgpt_jwt_claims;
use sha2::Digest;
use sha2::Sha256;
use tracing::debug;

const LEDGER_FILE_NAME: &str = "task_usage_weekly.json";
const UNKNOWN_SCOPE_ID: &str = "unknown";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TaskUsageScope {
    id: String,
    file_suffix: String,
}

impl TaskUsageScope {
    pub(super) fn discover(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        keyring_backend_kind: AuthKeyringBackendKind,
    ) -> Self {
        let id = read_codex_api_key_from_env()
            .or_else(read_openai_api_key_from_env)
            .map(|api_key| format!("api-key:{}", fingerprint(api_key.as_bytes())))
            .or_else(|| {
                read_codex_access_token_from_env()
                    .and_then(|token| account_id_from_access_token(&token))
                    .map(|account_id| format!("chatgpt:{account_id}"))
            })
            .or_else(|| {
                load_auth_dot_json(
                    codex_home,
                    auth_credentials_store_mode,
                    keyring_backend_kind,
                )
                .ok()
                .flatten()
                .and_then(account_scope_id_from_auth)
            })
            .unwrap_or_else(|| UNKNOWN_SCOPE_ID.to_string());

        let file_suffix = if id == UNKNOWN_SCOPE_ID {
            UNKNOWN_SCOPE_ID.to_string()
        } else {
            fingerprint(id.as_bytes())
        };
        debug!(scope = %redact_scope_id(&id), "selected task usage account scope");
        Self { id, file_suffix }
    }

    pub(super) fn ledger_path(&self, codex_home: &Path) -> PathBuf {
        if self.id == UNKNOWN_SCOPE_ID {
            codex_home.join(LEDGER_FILE_NAME)
        } else {
            codex_home.join(format!("task_usage_weekly.{}.json", self.file_suffix))
        }
    }

    pub(super) fn accepts_persisted_id(&self, persisted_id: Option<&str>) -> bool {
        match persisted_id {
            Some(persisted_id) => persisted_id == self.id,
            None => self.id == UNKNOWN_SCOPE_ID,
        }
    }

    pub(super) fn persisted_id(&self) -> Option<String> {
        (self.id != UNKNOWN_SCOPE_ID).then(|| self.id.clone())
    }
}

fn account_scope_id_from_auth(auth: AuthDotJson) -> Option<String> {
    if let Some(account_id) = auth.tokens.as_ref().and_then(|tokens| {
        tokens
            .account_id
            .clone()
            .or_else(|| tokens.id_token.chatgpt_account_id.clone())
            .or_else(|| account_id_from_access_token(&tokens.access_token))
    }) {
        return Some(format!("chatgpt:{account_id}"));
    }

    if let Some(agent_identity) = auth.agent_identity {
        let account_id = match agent_identity {
            AgentIdentityStorage::Record(record) => Some(record.account_id),
            AgentIdentityStorage::Jwt(jwt) => account_id_from_agent_identity_jwt(&jwt),
        };
        if let Some(account_id) = account_id {
            return Some(format!("chatgpt:{account_id}"));
        }
    }

    if let Some(api_key) = auth.openai_api_key {
        return Some(format!("api-key:{}", fingerprint(api_key.as_bytes())));
    }

    auth.personal_access_token
        .map(|token| format!("personal-access-token:{}", fingerprint(token.as_bytes())))
}

fn account_id_from_agent_identity_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&payload)
        .ok()?
        .get("account_id")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn account_id_from_access_token(token: &str) -> Option<String> {
    parse_chatgpt_jwt_claims(token)
        .ok()
        .and_then(|claims| claims.chatgpt_account_id)
}

fn fingerprint(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn redact_scope_id(scope_id: &str) -> String {
    scope_id
        .strip_prefix("chatgpt:")
        .map(|account_id| format!("chatgpt:{}", fingerprint(account_id.as_bytes())))
        .unwrap_or_else(|| scope_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_login::token_data::TokenData;
    use codex_protocol::auth::AuthMode;

    #[test]
    fn chatgpt_scope_uses_workspace_account_id() {
        let auth = AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                account_id: Some("workspace-1".to_string()),
                ..TokenData::default()
            }),
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        };

        assert_eq!(
            account_scope_id_from_auth(auth),
            Some("chatgpt:workspace-1".to_string())
        );
    }

    #[test]
    fn unscoped_legacy_records_are_only_accepted_for_unknown_scope() {
        let unknown = TaskUsageScope {
            id: UNKNOWN_SCOPE_ID.to_string(),
            file_suffix: UNKNOWN_SCOPE_ID.to_string(),
        };
        let account = TaskUsageScope {
            id: "chatgpt:workspace-1".to_string(),
            file_suffix: "scope".to_string(),
        };

        assert!(unknown.accepts_persisted_id(None));
        assert!(!account.accepts_persisted_id(None));
    }
}
