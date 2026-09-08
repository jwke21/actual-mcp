use prost::Message;

use crate::actual::messages::{SyncMessage, decode_response};
use crate::actual::model::{ApiError, Envelope, LoginData, Snapshot};
use crate::actual::proto::SyncRequest;
use crate::actual::{error::ActualError, model::UserFile};
use crate::config::{Config, Secret};

const DEFAULT_ACTUAL_CLIENT_TIMEOUT: u64 = 30;
const ACTUAL_TOKEN_HEADER: &str = "X-ACTUAL-TOKEN";
const ACTUAL_FILE_ID_HEADER: &str = "X-ACTUAL-FILE-ID";

pub struct ActualClient {
    http: reqwest::Client,
    base: String,
    // kept in client in case we need to re-authenticate
    password: Secret,
    token: tokio::sync::RwLock<Option<String>>,
}

impl ActualClient {
    pub fn new(config: &Config) -> Result<Self, ActualError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                DEFAULT_ACTUAL_CLIENT_TIMEOUT,
            ))
            .build()
            .map_err(|e| ActualError::Transport {
                url: config.server_url.clone(),
                source: e,
            })?;
        Ok(ActualClient {
            http,
            base: config.server_url.clone(),
            password: config.password.clone(),
            token: tokio::sync::RwLock::new(None),
        })
    }

    pub async fn login(&self) -> Result<(), ActualError> {
        let req = self
            .http
            .post(format!("{}/account/login", self.base))
            .json(&serde_json::json!({"password": self.password.expose()}));

        let data: LoginData = self.send("/account/login", req).await?;
        *self.token.write().await = Some(data.token);
        Ok(())
    }

    pub async fn list_files(&self) -> Result<Vec<UserFile>, ActualError> {
        let token = self.token().await?;
        let req = self
            .http
            .get(format!("{}/sync/list-user-files", self.base))
            .header(ACTUAL_TOKEN_HEADER, token);
        self.send("/sync/list-user-files", req).await
    }

    // imperative shell
    pub async fn select_budget(&self, sync_id: Option<&str>) -> Result<UserFile, ActualError> {
        select(self.list_files().await?, sync_id)
    }

    /// Helper to handle lazy authentication
    async fn token(&self) -> Result<String, ActualError> {
        let cached = self.token.read().await.clone(); // bind to trop the guard
        if let Some(t) = cached {
            return Ok(t);
        }
        self.login().await?;
        self.token
            .read()
            .await
            .clone()
            .ok_or(ActualError::Auth("no token after login".into()))
    }

    /// Helper function to handle sending requests, decoding responses, and mapping errors
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &'static str,
        req: reqwest::RequestBuilder,
    ) -> Result<T, ActualError> {
        let resp = req.send().await.map_err(|e| ActualError::Transport {
            url: format!("{}{endpoint}", self.base),
            source: e,
        })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| ActualError::Transport {
            url: format!("{}{endpoint}", self.base),
            source: e,
        })?;

        match serde_json::from_str::<Envelope<T>>(&body) {
            Ok(Envelope::Success { data }) => Ok(data),
            Ok(Envelope::Failure { reason, .. }) => Err(
                if status == reqwest::StatusCode::UNAUTHORIZED || reason == "invalid-password" {
                    ActualError::Auth(reason)
                } else {
                    ActualError::Api {
                        status: status.as_u16(),
                        reason,
                    }
                },
            ),
            Err(source) => Err(ActualError::Decode { endpoint, source }),
        }
    }

    pub async fn download_snapshot(&self, file_id: &str) -> Result<Snapshot, ActualError> {
        let token = self.token().await?;
        let req = self
            .http
            .get(format!("{}/sync/download-user-file", self.base))
            .header(ACTUAL_TOKEN_HEADER, token)
            .header(ACTUAL_FILE_ID_HEADER, file_id);

        let bytes = self.send_bytes("/sync/download-user-file", req).await?;
        crate::actual::snapshot::parse(&bytes)
    }

    pub async fn fetch_messages(
        &self,
        file_id: &str,
        group_id: &str,
        since: &str,
    ) -> Result<Vec<SyncMessage>, ActualError> {
        let token = self.token().await?;
        let body = SyncRequest {
            messages: Vec::new(),
            file_id: file_id.to_string(),
            group_id: group_id.to_string(),
            key_id: String::new(),
            since: since.to_string(),
        }
        .encode_to_vec();

        let req = self
            .http
            .post(format!("{}/sync/sync", self.base))
            .header(ACTUAL_TOKEN_HEADER, token)
            .header(reqwest::header::CONTENT_TYPE, "application/actual-sync")
            .body(body);

        let bytes = self.send_bytes("/sync/sync", req).await?;
        decode_response(&bytes)
    }

    async fn send_bytes(
        &self,
        endpoint: &'static str,
        req: reqwest::RequestBuilder,
    ) -> Result<Vec<u8>, ActualError> {
        let resp = req.send().await.map_err(|e| ActualError::Transport {
            url: format!("{}{endpoint}", self.base),
            source: e,
        })?;

        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.map_err(|e| ActualError::Transport {
                url: format!("{}{endpoint}", self.base),
                source: e,
            })?;

            // errors are envelopes even on this binary route but not always
            return Err(match serde_json::from_str::<ApiError>(&body) {
                Ok(ApiError { reason }) if status == reqwest::StatusCode::UNAUTHORIZED => {
                    ActualError::Auth(reason)
                }
                Ok(ApiError { reason }) => ActualError::Api {
                    status: status.as_u16(),
                    reason,
                },
                Err(_) => ActualError::Api {
                    status: status.as_u16(),
                    reason: body,
                },
            });
        }

        let bytes = resp.bytes().await.map_err(|e| ActualError::Transport {
            url: format!("{}{endpoint}", self.base),
            source: e,
        })?;
        Ok(bytes.to_vec())
    }
}

/// Functional core: choose the budget to work with, given everything the
/// server listed. Pure — no network, so every branch is unit-testable.
pub(crate) fn select(files: Vec<UserFile>, sync_id: Option<&str>) -> Result<UserFile, ActualError> {
    // 1. the server lists deleted budgets too, filter them out first so a
    //    deleted file can never make a single-budget server look ambiguous
    let live: Vec<UserFile> = files.into_iter().filter(|f| f.deleted == 0).collect();

    // 2. pick one
    let chosen = match sync_id {
        Some(id) => {
            // capture what is on offer before `find` consumes `live`
            let available: Vec<String> = live.iter().filter_map(|f| f.group_id.clone()).collect();
            live.into_iter()
                .find(|f| f.group_id.as_deref() == Some(id))
                .ok_or(ActualError::BudgetNotFound {
                    sync_id: id.to_string(),
                    available,
                })?
        }
        None => match live.len() {
            0 => return Err(ActualError::NoBudget),
            1 => live.into_iter().next().expect("len checked above"),
            _ => {
                return Err(ActualError::AmbiguousBudget {
                    names: live.into_iter().map(|f| f.name).collect(),
                });
            }
        },
    };

    // 3. encryption check LAST, on the chosen file only: an encrypted budget
    //    we were never going to use must not fail the whole operation
    if chosen.encrypt_key_id.is_some() {
        return Err(ActualError::Encrypted { name: chosen.name });
    }

    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live, unencrypted budget. Mutate fields for the special cases.
    fn budget(name: &str, group: &str) -> UserFile {
        UserFile {
            file_id: format!("file-{name}"),
            group_id: Some(group.to_string()),
            name: name.to_string(),
            encrypt_key_id: None,
            deleted: 0,
        }
    }

    #[test]
    fn picks_sole_live_budget() {
        let got = select(
            vec![budget(
                "Test Budget",
                "22222222-2222-2222-2222-222222222222",
            )],
            None,
        )
        .unwrap();
        assert_eq!(got.name, "Test Budget");
    }

    #[test]
    fn deleted_budgets_are_ignored() {
        let mut old = budget("Old", "g-old");
        old.deleted = 1;
        let got = select(vec![old, budget("Live", "g-live")], None).unwrap();
        assert_eq!(got.name, "Live");
    }

    #[test]
    fn empty_list_is_no_budget() {
        assert!(matches!(select(vec![], None), Err(ActualError::NoBudget)));
    }

    #[test]
    fn all_deleted_is_no_budget() {
        let mut only = budget("Old", "g-old");
        only.deleted = 1;
        assert!(matches!(
            select(vec![only], None),
            Err(ActualError::NoBudget)
        ));
    }

    #[test]
    fn matches_on_group_id() {
        let files = vec![budget("A", "g-a"), budget("B", "g-b")];
        assert_eq!(select(files, Some("g-b")).unwrap().name, "B");
    }

    /// The sync id in Actual's UI is the *group* id, never the file id.
    #[test]
    fn sync_id_does_not_match_file_id() {
        let files = vec![budget("A", "g-a")];
        assert!(matches!(
            select(files, Some("file-A")),
            Err(ActualError::BudgetNotFound { .. })
        ));
    }

    #[test]
    fn deleted_budget_is_not_selectable_by_sync_id() {
        let mut old = budget("Old", "g-old");
        old.deleted = 1;
        assert!(matches!(
            select(vec![old], Some("g-old")),
            Err(ActualError::BudgetNotFound { .. })
        ));
    }

    #[test]
    fn many_without_sync_id_is_ambiguous() {
        let files = vec![budget("A", "g-a"), budget("B", "g-b")];
        match select(files, None) {
            Err(ActualError::AmbiguousBudget { names }) => assert_eq!(names, vec!["A", "B"]),
            other => panic!("expected AmbiguousBudget, got {other:?}"),
        }
    }

    /// The error must name the ids the user could actually set.
    #[test]
    fn unknown_sync_id_reports_available_group_ids() {
        let files = vec![budget("A", "g-a"), budget("B", "g-b")];
        match select(files, Some("nope")) {
            Err(ActualError::BudgetNotFound { sync_id, available }) => {
                assert_eq!(sync_id, "nope");
                assert_eq!(available, vec!["g-a", "g-b"]);
            }
            other => panic!("expected BudgetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn encrypted_chosen_budget_is_rejected() {
        let mut b = budget("Secret", "g-s");
        b.encrypt_key_id = Some("key-1".to_string());
        match select(vec![b], None) {
            Err(ActualError::Encrypted { name }) => assert_eq!(name, "Secret"),
            other => panic!("expected Encrypted, got {other:?}"),
        }
    }

    /// Guards the ordering rule in step 3. If the encryption check is ever
    /// "tidied" into the filter in step 1, this test fails.
    #[test]
    fn encrypted_other_budget_does_not_block_selection() {
        let mut enc = budget("Encrypted", "g-enc");
        enc.encrypt_key_id = Some("key-1".to_string());
        let got = select(vec![enc, budget("Plain", "g-plain")], Some("g-plain")).unwrap();
        assert_eq!(got.name, "Plain");
    }
}
