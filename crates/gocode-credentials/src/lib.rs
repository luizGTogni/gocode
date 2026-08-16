use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// A credential value that deliberately omits its contents from debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps one secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the secret only to the code that must hand it to an authenticated client.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

/// Minimal synchronous boundary around a native credential store.
pub trait CredentialStore: Send + Sync {
    /// Gets the saved NVIDIA credential, if present.
    ///
    /// # Errors
    ///
    /// Returns an error reported by the native credential service.
    fn get_nvidia(&self) -> Result<Option<SecretString>, CredentialStoreError>;

    /// Persists a credential after an explicit user choice in onboarding.
    ///
    /// # Errors
    ///
    /// Returns an error reported by the native credential service.
    fn save_nvidia(&self, secret: &SecretString) -> Result<(), CredentialStoreError>;
}

/// A failure reported by the operating system's credential service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStoreError {
    /// The credential service is not available for this user session.
    Unavailable(String),
    /// The service responded but the requested entry could not be read.
    ReadFailed(String),
}

/// Resolves environment credentials before consulting the native store.
pub struct CredentialResolver<S> {
    store: S,
}

impl<S> CredentialResolver<S>
where
    S: CredentialStore,
{
    /// Creates a resolver with one native credential store.
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    /// Resolves an environment credential first, avoiding any store access when it is present.
    ///
    /// # Errors
    ///
    /// Returns the native-store failure when no environment value exists and the store is
    /// unavailable.
    pub fn resolve(
        &self,
        environment_value: Option<&str>,
    ) -> Result<SecretString, CredentialStoreError> {
        if let Some(value) = environment_value.filter(|value| !value.is_empty()) {
            return Ok(SecretString::new(value));
        }

        self.store.get_nvidia()?.ok_or_else(|| {
            CredentialStoreError::Unavailable("no NVIDIA credential is stored".into())
        })
    }
}

/// In-memory credential store used by deterministic tests.
#[derive(Clone)]
pub struct FakeCredentialStore {
    secret: Arc<Mutex<Option<SecretString>>>,
    get_calls: Arc<AtomicUsize>,
    save_calls: Arc<AtomicUsize>,
}

impl FakeCredentialStore {
    /// Creates a fake store containing one NVIDIA secret.
    #[must_use]
    pub fn with_secret(secret: impl Into<String>) -> Self {
        Self {
            secret: Arc::new(Mutex::new(Some(SecretString::new(secret)))),
            get_calls: Arc::new(AtomicUsize::new(0)),
            save_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a fake store with no saved secret.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            secret: Arc::new(Mutex::new(None)),
            get_calls: Arc::new(AtomicUsize::new(0)),
            save_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns how often the fake was consulted.
    #[must_use]
    pub fn get_calls(&self) -> usize {
        self.get_calls.load(Ordering::Relaxed)
    }

    /// Returns how often the fake was explicitly asked to save a credential.
    #[must_use]
    pub fn save_calls(&self) -> usize {
        self.save_calls.load(Ordering::Relaxed)
    }
}

impl CredentialStore for FakeCredentialStore {
    fn get_nvidia(&self) -> Result<Option<SecretString>, CredentialStoreError> {
        self.get_calls.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .secret
            .lock()
            .expect("fake store mutex should not poison")
            .clone())
    }

    fn save_nvidia(&self, secret: &SecretString) -> Result<(), CredentialStoreError> {
        self.save_calls.fetch_add(1, Ordering::Relaxed);
        *self
            .secret
            .lock()
            .expect("fake store mutex should not poison") = Some(secret.clone());
        Ok(())
    }
}

/// A deterministic unavailable-store implementation for recovery paths and tests.
#[derive(Clone)]
pub struct UnavailableCredentialStore {
    reason: String,
    get_calls: Arc<AtomicUsize>,
}

impl UnavailableCredentialStore {
    /// Creates an unavailable store with an actionable diagnostic reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            get_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns how often the store was consulted.
    #[must_use]
    pub fn get_calls(&self) -> usize {
        self.get_calls.load(Ordering::Relaxed)
    }
}

impl CredentialStore for UnavailableCredentialStore {
    fn get_nvidia(&self) -> Result<Option<SecretString>, CredentialStoreError> {
        self.get_calls.fetch_add(1, Ordering::Relaxed);
        Err(CredentialStoreError::Unavailable(self.reason.clone()))
    }

    fn save_nvidia(&self, _secret: &SecretString) -> Result<(), CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(self.reason.clone()))
    }
}

/// Native OS credential store for the default NVIDIA credential profile.
///
/// On Windows this uses Credential Manager. On Linux the selected `keyring` backend is
/// Secret Service. Constructing this value performs no system credential operation; only
/// [`CredentialStore::get_nvidia`] accesses the store, once per explicit resolution.
pub struct NativeCredentialStore {
    service: &'static str,
    account: &'static str,
}

impl NativeCredentialStore {
    /// Creates the default store identity without contacting the operating system.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            service: "gocode",
            account: "nvidia/default",
        }
    }

    /// Returns the native-store service name.
    #[must_use]
    pub const fn service_name(&self) -> &'static str {
        self.service
    }

    /// Returns the native-store account name.
    #[must_use]
    pub const fn account_name(&self) -> &'static str {
        self.account
    }

    /// Gets a secret stored under an arbitrary account name (e.g. `"mcp/<server>"`), under the
    /// same service identity as [`Self::get_nvidia`]. Unlike the NVIDIA slot, callers choose
    /// their own account naming — used for per-MCP-server API keys and OAuth tokens.
    ///
    /// # Errors
    ///
    /// Returns an error reported by the native credential service.
    pub fn get_secret(&self, account: &str) -> Result<Option<SecretString>, CredentialStoreError> {
        let entry = keyring::Entry::new(self.service, account)
            .map_err(|error| CredentialStoreError::ReadFailed(error.to_string()))?;

        match entry.get_password() {
            Ok(value) => Ok(Some(SecretString::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(
                keyring::Error::NoStorageAccess(error) | keyring::Error::PlatformFailure(error),
            ) => Err(CredentialStoreError::Unavailable(error.to_string())),
            Err(error) => Err(CredentialStoreError::ReadFailed(error.to_string())),
        }
    }

    /// Persists a secret under an arbitrary account name. See [`Self::get_secret`].
    ///
    /// # Errors
    ///
    /// Returns an error reported by the native credential service.
    pub fn save_secret(
        &self,
        account: &str,
        secret: &SecretString,
    ) -> Result<(), CredentialStoreError> {
        let entry = keyring::Entry::new(self.service, account)
            .map_err(|error| CredentialStoreError::ReadFailed(error.to_string()))?;

        entry
            .set_password(secret.expose())
            .map_err(|error| match error {
                keyring::Error::NoStorageAccess(error) | keyring::Error::PlatformFailure(error) => {
                    CredentialStoreError::Unavailable(error.to_string())
                }
                other => CredentialStoreError::ReadFailed(other.to_string()),
            })
    }

    /// Removes a secret stored under an arbitrary account name, if present. See
    /// [`Self::get_secret`].
    ///
    /// # Errors
    ///
    /// Returns an error reported by the native credential service; a missing entry is not an
    /// error.
    pub fn delete_secret(&self, account: &str) -> Result<(), CredentialStoreError> {
        let entry = keyring::Entry::new(self.service, account)
            .map_err(|error| CredentialStoreError::ReadFailed(error.to_string()))?;

        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(
                keyring::Error::NoStorageAccess(error) | keyring::Error::PlatformFailure(error),
            ) => Err(CredentialStoreError::Unavailable(error.to_string())),
            Err(error) => Err(CredentialStoreError::ReadFailed(error.to_string())),
        }
    }
}

impl Default for NativeCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for NativeCredentialStore {
    fn get_nvidia(&self) -> Result<Option<SecretString>, CredentialStoreError> {
        let entry = keyring::Entry::new(self.service, self.account)
            .map_err(|error| CredentialStoreError::ReadFailed(error.to_string()))?;

        match entry.get_password() {
            Ok(value) => Ok(Some(SecretString::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(
                keyring::Error::NoStorageAccess(error) | keyring::Error::PlatformFailure(error),
            ) => Err(CredentialStoreError::Unavailable(error.to_string())),
            Err(error) => Err(CredentialStoreError::ReadFailed(error.to_string())),
        }
    }

    fn save_nvidia(&self, secret: &SecretString) -> Result<(), CredentialStoreError> {
        let entry = keyring::Entry::new(self.service, self.account)
            .map_err(|error| CredentialStoreError::ReadFailed(error.to_string()))?;

        entry
            .set_password(secret.expose())
            .map_err(|error| match error {
                keyring::Error::NoStorageAccess(error) | keyring::Error::PlatformFailure(error) => {
                    CredentialStoreError::Unavailable(error.to_string())
                }
                other => CredentialStoreError::ReadFailed(other.to_string()),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::CredentialStore;

    #[test]
    fn secret_debug_output_is_redacted() {
        let secret = super::SecretString::new("nvapi-secret-value");

        assert!(!format!("{secret:?}").contains("nvapi-secret-value"));
    }

    #[test]
    fn environment_credential_wins_without_touching_the_store() {
        let store = super::FakeCredentialStore::with_secret("stored-secret");
        let resolver = super::CredentialResolver::new(store.clone());

        let secret = resolver
            .resolve(Some("environment-secret"))
            .expect("environment credentials should resolve");

        assert_eq!(secret.expose(), "environment-secret");
        assert_eq!(store.get_calls(), 0);
    }

    #[test]
    fn unavailable_store_is_consulted_once_without_retrying() {
        let store = super::UnavailableCredentialStore::new("Secret Service unavailable");
        let resolver = super::CredentialResolver::new(store.clone());

        let error = resolver
            .resolve(None)
            .expect_err("an unavailable store should not yield a credential");

        assert!(matches!(error, super::CredentialStoreError::Unavailable(_)));
        assert_eq!(store.get_calls(), 1);
    }

    #[test]
    fn native_store_uses_a_stable_nvidia_entry_identity() {
        let store = super::NativeCredentialStore::new();

        assert_eq!(store.service_name(), "gocode");
        assert_eq!(store.account_name(), "nvidia/default");
    }

    #[test]
    fn explicit_save_writes_the_native_entry_once() {
        let store = super::FakeCredentialStore::empty();

        store
            .save_nvidia(&super::SecretString::new("onboarding-secret"))
            .expect("explicit save should succeed");

        assert_eq!(store.save_calls(), 1);
        assert_eq!(
            store.get_nvidia().expect("fake store should be readable"),
            Some(super::SecretString::new("onboarding-secret"))
        );
    }
}
