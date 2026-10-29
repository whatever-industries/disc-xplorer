use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime, State};
use uuid::Uuid;

const ACTIVATION_ENDPOINT: &str =
    "https://disc-xplorer-activation-live.scriptsynch.workers.dev/v1/activations";
const RECEIPT_ISSUER: &str = "https://disc-xplorer-activation-live.scriptsynch.workers.dev";
const RECEIPT_AUDIENCE: &str = "disc-xplorer-supporter";
const RECEIPT_KEY_ID: &str = "disc-xplorer-live-2026-10-27";
const RECEIPT_PUBLIC_KEY_HEX: &str =
    "d6b4cd2076e7cb21589e90517703e39a095e445e7265a0869d113b1c4ab26a93";
const STORE_FILE: &str = "donation-activation-v1.json";
const RETRY_MESSAGE: &str =
    "We couldn't confirm that donation yet. Try again shortly or contact workflowdept@gmail.com.";
const STORAGE_MESSAGE: &str =
    "Activation could not be saved locally. Check available storage, then try again.";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationStatus {
    pub activated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationResult {
    pub activated: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActivationStore {
    version: u8,
    installation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt: Option<String>,
}

#[derive(Default)]
struct AttemptInner {
    generation: u64,
    active: bool,
    pending: Option<PendingAttempt>,
}

#[derive(Clone)]
struct PendingAttempt {
    email: String,
    operation_id: String,
}

#[derive(Default)]
pub struct ActivationAttempts(Mutex<AttemptInner>);

impl ActivationAttempts {
    fn start(&self, email: String) -> Result<(u64, String), String> {
        let mut inner = self.0.lock().map_err(|_| "Activation is unavailable.")?;
        if inner.active {
            return Err("Wait for the current activation request.".into());
        }
        let operation_id = match &inner.pending {
            Some(pending) if pending.email == email => pending.operation_id.clone(),
            _ => Uuid::new_v4().to_string(),
        };
        inner.pending = Some(PendingAttempt {
            email,
            operation_id: operation_id.clone(),
        });
        inner.generation = inner.generation.wrapping_add(1);
        inner.active = true;
        Ok((inner.generation, operation_id))
    }

    fn current(&self, generation: u64) -> bool {
        self.0
            .lock()
            .map(|inner| inner.generation == generation && inner.active)
            .unwrap_or(false)
    }

    fn retryable(&self, generation: u64) {
        if let Ok(mut inner) = self.0.lock() {
            if inner.generation == generation {
                inner.active = false;
            }
        }
    }

    fn finish(&self, generation: u64) {
        if let Ok(mut inner) = self.0.lock() {
            if inner.generation == generation {
                inner.active = false;
                inner.pending = None;
            }
        }
    }

    fn cancel(&self) {
        if let Ok(mut inner) = self.0.lock() {
            inner.generation = inner.generation.wrapping_add(1);
            inner.active = false;
            inner.pending = None;
        }
    }
}

fn store_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|dir| dir.join(STORE_FILE))
        .map_err(|_| STORAGE_MESSAGE.into())
}

fn valid_uuid(value: &str) -> bool {
    Uuid::parse_str(value)
        .map(|id| id.hyphenated().to_string() == value && id.get_version_num() == 4)
        .unwrap_or(false)
}

fn load_store(path: &Path) -> Result<Option<ActivationStore>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(path).map_err(|_| STORAGE_MESSAGE.to_string())?;
    if raw.len() > 8 * 1024 {
        return Err(STORAGE_MESSAGE.into());
    }
    let store: ActivationStore =
        serde_json::from_slice(&raw).map_err(|_| STORAGE_MESSAGE.to_string())?;
    if store.version != 1 || !valid_uuid(&store.installation_id) {
        return Err(STORAGE_MESSAGE.into());
    }
    if store
        .receipt
        .as_ref()
        .is_some_and(|receipt| receipt.len() > 4096)
    {
        return Err(STORAGE_MESSAGE.into());
    }
    Ok(Some(store))
}

fn write_store(path: &Path, store: &ActivationStore) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| STORAGE_MESSAGE.to_string())?;
    fs::create_dir_all(parent).map_err(|_| STORAGE_MESSAGE.to_string())?;
    let encoded = serde_json::to_vec(store).map_err(|_| STORAGE_MESSAGE.to_string())?;
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let mut file = File::create(&temp).map_err(|_| STORAGE_MESSAGE.to_string())?;
        file.write_all(&encoded)
            .map_err(|_| STORAGE_MESSAGE.to_string())?;
        file.sync_all().map_err(|_| STORAGE_MESSAGE.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
                .map_err(|_| STORAGE_MESSAGE.to_string())?;
        }
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path).map_err(|_| STORAGE_MESSAGE.to_string())?;
        }
        fs::rename(&temp, path).map_err(|_| STORAGE_MESSAGE.to_string())?;
        let confirmed = fs::read(path).map_err(|_| STORAGE_MESSAGE.to_string())?;
        if confirmed != encoded {
            return Err(STORAGE_MESSAGE.into());
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

fn load_or_create_store(path: &Path) -> Result<ActivationStore, String> {
    if let Some(store) = load_store(path)? {
        return Ok(store);
    }
    let store = ActivationStore {
        version: 1,
        installation_id: Uuid::new_v4().to_string(),
        receipt: None,
    };
    write_store(path, &store)?;
    Ok(store)
}

fn activated_from_store(path: &Path) -> Result<bool, String> {
    let store = load_or_create_store(path)?;
    Ok(store
        .receipt
        .as_deref()
        .is_some_and(|receipt| verify_receipt(receipt, &store.installation_id).is_ok()))
}

fn decode_segment(segment: &str) -> Result<Vec<u8>, String> {
    if segment.is_empty()
        || !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("Invalid recognition receipt.".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| "Invalid recognition receipt.")?;
    if URL_SAFE_NO_PAD.encode(&bytes) != segment {
        return Err("Invalid recognition receipt.".into());
    }
    Ok(bytes)
}

fn json_object(segment: &str) -> Result<serde_json::Map<String, Value>, String> {
    let bytes = decode_segment(segment)?;
    if bytes.len() > 2048 || bytes.iter().any(|byte| !(0x20..=0x7e).contains(byte)) {
        return Err("Invalid recognition receipt.".into());
    }
    match serde_json::from_slice::<Value>(&bytes).map_err(|_| "Invalid recognition receipt.")? {
        Value::Object(value) => Ok(value),
        _ => Err("Invalid recognition receipt.".into()),
    }
}

fn exact_keys(value: &serde_json::Map<String, Value>, expected: &[&str]) -> bool {
    let actual: BTreeSet<&str> = value.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    actual == expected
}

fn string_claim<'a>(
    claims: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    claims
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| "Recognition receipt does not match this installation.".into())
}

fn receipt_key() -> Result<VerifyingKey, String> {
    if RECEIPT_PUBLIC_KEY_HEX.len() != 64 {
        return Err("Unsupported recognition signature.".into());
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&RECEIPT_PUBLIC_KEY_HEX[index * 2..index * 2 + 2], 16)
            .map_err(|_| "Unsupported recognition signature.")?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| "Unsupported recognition signature.".into())
}

struct ReceiptVerifier<'a> {
    issuer: &'a str,
    audience: &'a str,
    key_id: &'a str,
    key: VerifyingKey,
}

fn verify_receipt_with(
    receipt: &str,
    installation_id: &str,
    verifier: &ReceiptVerifier<'_>,
) -> Result<(), String> {
    if receipt.len() > 4096 || !valid_uuid(installation_id) {
        return Err("Invalid recognition receipt.".into());
    }
    let parts: Vec<&str> = receipt.split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid recognition receipt.".into());
    }
    let header = json_object(parts[0])?;
    let claims = json_object(parts[1])?;
    if !exact_keys(&header, &["alg", "kid", "typ"])
        || header.get("alg").and_then(Value::as_str) != Some("EdDSA")
        || header.get("typ").and_then(Value::as_str) != Some("JWT")
        || header.get("kid").and_then(Value::as_str) != Some(verifier.key_id)
    {
        return Err("Unsupported recognition signature.".into());
    }
    let signature = decode_segment(parts[2])?;
    if signature.len() != 64 {
        return Err("Recognition signature does not match.".into());
    }
    let signature =
        Signature::from_slice(&signature).map_err(|_| "Recognition signature does not match.")?;
    let signed = format!("{}.{}", parts[0], parts[1]);
    verifier
        .key
        .verify_strict(signed.as_bytes(), &signature)
        .map_err(|_| "Recognition signature does not match.")?;
    if !exact_keys(&claims, &["v", "iss", "aud", "sub", "iat", "jti", "iid"])
        || claims.get("v").and_then(Value::as_u64) != Some(1)
        || string_claim(&claims, "iss")? != verifier.issuer
        || string_claim(&claims, "aud")? != verifier.audience
        || !valid_uuid(string_claim(&claims, "sub")?)
        || !valid_uuid(string_claim(&claims, "jti")?)
        || string_claim(&claims, "iid")? != installation_id
        || claims
            .get("iat")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .is_none()
    {
        return Err("Recognition receipt does not match this installation.".into());
    }
    Ok(())
}

fn verify_receipt(receipt: &str, installation_id: &str) -> Result<(), String> {
    let verifier = ReceiptVerifier {
        issuer: RECEIPT_ISSUER,
        audience: RECEIPT_AUDIENCE,
        key_id: RECEIPT_KEY_ID,
        key: receipt_key()?,
    };
    verify_receipt_with(receipt, installation_id, &verifier)
}

fn normalized_email(email: &str) -> Result<String, String> {
    let value = email.trim();
    if value.is_empty() || value.len() > 254 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("Enter the email used for the donation.".into());
    }
    Ok(value.to_string())
}

fn client() -> Result<Client, String> {
    Client::builder()
        .redirect(Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| RETRY_MESSAGE.into())
}

#[tauri::command]
pub fn activation_status<R: Runtime>(app: AppHandle<R>) -> Result<ActivationStatus, String> {
    let path = store_path(&app)?;
    Ok(ActivationStatus {
        activated: activated_from_store(&path)?,
    })
}

#[tauri::command]
pub fn cancel_donation_activation(attempts: State<'_, ActivationAttempts>) {
    attempts.cancel();
}

#[tauri::command]
pub async fn verify_donation<R: Runtime>(
    app: AppHandle<R>,
    email: String,
    attempts: State<'_, ActivationAttempts>,
) -> Result<ActivationResult, String> {
    let email = normalized_email(&email)?;
    let (generation, operation_id) = attempts.start(email.clone())?;
    let run = async {
        let path = store_path(&app)?;
        let mut store = load_or_create_store(&path)?;
        let request = serde_json::json!({
            "email": email,
            "installationId": store.installation_id,
            "operationId": operation_id,
        });
        let response = client()?
            .post(ACTIVATION_ENDPOINT)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|_| RETRY_MESSAGE.to_string())?;
        if response.status() != StatusCode::OK {
            return Err(RETRY_MESSAGE.into());
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| RETRY_MESSAGE.to_string())?;
        if body.len() > 8192 {
            return Err(RETRY_MESSAGE.into());
        }
        let response: Value =
            serde_json::from_slice(&body).map_err(|_| RETRY_MESSAGE.to_string())?;
        if !attempts.current(generation) {
            return Err("Activation cancelled.".into());
        }
        let response = response
            .as_object()
            .ok_or_else(|| RETRY_MESSAGE.to_string())?;
        match response.get("status").and_then(Value::as_str) {
            Some("not_found") if exact_keys(response, &["status"]) => Ok(ActivationResult {
                activated: false,
                message: RETRY_MESSAGE.into(),
            }),
            Some("recognized") if exact_keys(response, &["status", "receipt"]) => {
                let receipt = response
                    .get("receipt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RETRY_MESSAGE.to_string())?;
                verify_receipt(receipt, &store.installation_id)
                    .map_err(|_| RETRY_MESSAGE.to_string())?;
                if !attempts.current(generation) {
                    return Err("Activation cancelled.".into());
                }
                store.receipt = Some(receipt.to_string());
                write_store(&path, &store)?;
                verify_receipt(
                    store.receipt.as_deref().unwrap_or_default(),
                    &store.installation_id,
                )
                .map_err(|_| STORAGE_MESSAGE.to_string())?;
                Ok(ActivationResult {
                    activated: true,
                    message: "Thank you! This installation is permanently activated.".into(),
                })
            }
            _ => Err(RETRY_MESSAGE.into()),
        }
    }
    .await;
    match &run {
        Ok(_) => attempts.finish(generation),
        Err(_) => attempts.retryable(generation),
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn b64(value: &Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    }
    fn signed_receipt(key: &SigningKey, iid: &str, audience: &str) -> String {
        let header = b64(&json!({"alg":"EdDSA","kid":RECEIPT_KEY_ID,"typ":"JWT"}));
        let claims = b64(
            &json!({"v":1,"iss":RECEIPT_ISSUER,"aud":audience,"sub":"00000000-0000-4000-8000-000000000001","iat":1,"jti":"00000000-0000-4000-8000-000000000002","iid":iid}),
        );
        let signed = format!("{header}.{claims}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).to_bytes())
        )
    }

    #[test]
    fn receipt_verifies_only_its_product_signature_and_claims() {
        let iid = "00000000-0000-4000-8000-000000000003";
        let key = SigningKey::from_bytes(&[7; 32]);
        let verifier = ReceiptVerifier {
            issuer: RECEIPT_ISSUER,
            audience: RECEIPT_AUDIENCE,
            key_id: RECEIPT_KEY_ID,
            key: key.verifying_key(),
        };
        let receipt = signed_receipt(&key, iid, RECEIPT_AUDIENCE);
        assert!(verify_receipt_with(&receipt, iid, &verifier).is_ok());
        let other = signed_receipt(&key, iid, "script-supporter");
        assert!(verify_receipt_with(&other, iid, &verifier).is_err());
        let tampered = format!("{}A", &receipt[..receipt.len() - 1]);
        assert!(verify_receipt_with(&tampered, iid, &verifier).is_err());
    }

    #[test]
    fn retries_reuse_operation_and_cancellation_invalidates_reply() {
        let attempts = ActivationAttempts::default();
        let (first, operation) = attempts.start("person@example.com".into()).unwrap();
        attempts.retryable(first);
        let (retry, retried_operation) = attempts.start("person@example.com".into()).unwrap();
        assert_eq!(operation, retried_operation);
        attempts.cancel();
        assert!(!attempts.current(retry));
    }

    #[test]
    fn durable_store_round_trip_keeps_identity() {
        let path =
            std::env::temp_dir().join(format!("disc-xplorer-activation-{}.json", Uuid::new_v4()));
        let created = load_or_create_store(&path).unwrap();
        let loaded = load_or_create_store(&path).unwrap();
        assert_eq!(created.installation_id, loaded.installation_id);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn storage_failure_is_not_treated_as_activation() {
        let blocker = std::env::temp_dir().join(format!(
            "disc-xplorer-activation-blocker-{}",
            Uuid::new_v4()
        ));
        fs::write(&blocker, b"not a directory").unwrap();
        assert!(load_or_create_store(&blocker.join(STORE_FILE)).is_err());
        let _ = fs::remove_file(blocker);
    }

    #[test]
    #[ignore = "requires an explicitly supplied approved donation email and makes one live activation request"]
    fn live_activation_receipt_verifies_with_the_bundled_key() {
        let email = std::env::var("DX_ACTIVATION_EMAIL")
            .expect("set DX_ACTIVATION_EMAIL for the approved integration-test donation");
        let iid = Uuid::new_v4().to_string();
        let operation_id = Uuid::new_v4().to_string();
        let body = tauri::async_runtime::block_on(async {
            client().unwrap().post(ACTIVATION_ENDPOINT)
                .json(&serde_json::json!({"email":email,"installationId":iid,"operationId":operation_id}))
                .send().await.unwrap().bytes().await.unwrap()
        });
        let response: Value = serde_json::from_slice(&body).unwrap();
        let receipt = response
            .get("receipt")
            .and_then(Value::as_str)
            .expect("approved email must receive a receipt");
        verify_receipt(receipt, &iid).unwrap();
        let path =
            std::env::temp_dir().join(format!("disc-xplorer-offline-{}.json", Uuid::new_v4()));
        write_store(
            &path,
            &ActivationStore {
                version: 1,
                installation_id: iid,
                receipt: Some(receipt.into()),
            },
        )
        .unwrap();
        // This mirrors the next app launch: storage plus local signature checking only.
        assert!(activated_from_store(&path).unwrap());
        let _ = fs::remove_file(path);
    }
}
