//! Shared state for the auth routes.

use std::sync::Arc;

use rustmail_core::auth::{AuthService, CsrfProtector};

/// Cookie and proxy behaviour that depends on how the server is deployed.
#[derive(Debug, Clone, Copy)]
pub struct DeploymentSecurity {
    /// Mark cookies `Secure` (HTTPS only).
    pub cookie_secure: bool,
    /// Trust `X-Forwarded-For` / `X-Real-IP` for the client IP.
    pub trust_proxy_headers: bool,
}

/// What every auth handler, middleware and extractor needs.
///
/// Larger applications can embed this in their own state and implement
/// `FromRef` so [`AuthUser`](super::AuthUser) keeps working.
#[derive(Clone)]
pub struct AuthState {
    /// The authentication service.
    pub auth: AuthService,
    /// CSRF token issuer/verifier derived from the auth secret.
    pub csrf: CsrfProtector,
    /// Deployment-dependent security settings.
    pub security: Arc<DeploymentSecurity>,
}

impl AuthState {
    /// Builds the state from an [`AuthService`].
    ///
    /// # Errors
    /// Fails if the CSRF key cannot be derived.
    pub fn new(
        auth: AuthService,
        security: DeploymentSecurity,
    ) -> Result<Self, rustmail_core::Error> {
        let csrf = CsrfProtector::new(auth.config())?;
        Ok(Self {
            auth,
            csrf,
            security: Arc::new(security),
        })
    }
}
