use crate::identity::{IdentityError, IdentityStore, Role, SESSION_LIFETIME_SECONDS, User};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const COOKIE: &str = "panggung_session";
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_LIMIT: usize = 10;

#[derive(Clone)]
pub struct AuthState {
    identity: IdentityStore,
    authority: String,
    origin: String,
    attempts: Arc<Mutex<VecDeque<Instant>>>,
}

impl AuthState {
    pub fn new(identity: IdentityStore, port: u16) -> Self {
        let authority = format!("127.0.0.1:{port}");
        Self {
            origin: format!("http://{authority}"),
            authority,
            identity,
            attempts: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    // All connections are local; one bounded process-wide budget also limits
    // attempts that rotate usernames. No proxy headers are trusted.
    fn admit_login(&self) -> Result<(), StatusCode> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let now = Instant::now();
        while attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= LOGIN_WINDOW)
        {
            attempts.pop_front();
        }
        if attempts.len() >= LOGIN_LIMIT {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        attempts.push_back(now);
        Ok(())
    }
}

pub fn routes(state: AuthState) -> Router {
    Router::new()
        .route(
            "/api/auth/login",
            post(login).layer(DefaultBodyLimit::max(4096)),
        )
        .route("/api/auth/me", get(me))
        .route("/api/auth/logout", post(logout))
        .with_state(state)
}

pub fn protect(router: Router, state: AuthState) -> Router {
    router.route_layer(middleware::from_fn_with_state(state, authorize))
}

// Reject foreign Host values to prevent DNS rebinding, and require exact
// Origin for all mutations. Loopback HTTP intentionally has no Secure flag.
fn check_request(state: &AuthState, headers: &HeaderMap, mutation: bool) -> Result<(), StatusCode> {
    if headers.get_all(header::HOST).iter().count() != 1
        || headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            != Some(state.authority.as_str())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if mutation
        && (headers.get_all(header::ORIGIN).iter().count() != 1
            || headers
                .get(header::ORIGIN)
                .and_then(|value| value.to_str().ok())
                != Some(state.origin.as_str()))
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

fn session_token(headers: &HeaderMap) -> Result<&str, StatusCode> {
    let mut token = None;
    for value in headers.get_all(header::COOKIE) {
        for pair in value
            .to_str()
            .map_err(|_| StatusCode::UNAUTHORIZED)?
            .split(';')
        {
            if let Some((name, value)) = pair.trim().split_once('=')
                && name == COOKIE
                && token.replace(value).is_some()
            {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    }
    token.ok_or(StatusCode::UNAUTHORIZED)
}

fn identity_status(error: IdentityError) -> StatusCode {
    match error {
        IdentityError::InvalidCredentials | IdentityError::InvalidSession => {
            StatusCode::UNAUTHORIZED
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn principal(state: &AuthState, headers: &HeaderMap) -> Result<User, StatusCode> {
    state
        .identity
        .resolve_session(session_token(headers)?)
        .await
        .map_err(identity_status)
}

async fn authorize(
    State(state): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    check_request(&state, request.headers(), false)?;
    let user = principal(&state, request.headers()).await?;
    check_request(&state, request.headers(), mutation)?;
    if mutation && user.role == Role::Viewer {
        return Err(StatusCode::FORBIDDEN);
    }
    request.extensions_mut().insert(user);
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Result<Response, StatusCode> {
    check_request(&state, &headers, true)?;
    state.admit_login()?;
    let token = state
        .identity
        .login(&credentials.username, &credentials.password)
        .await
        .map_err(identity_status)?;
    let user = state
        .identity
        .resolve_session(token.as_str())
        .await
        .map_err(identity_status)?;
    // Rotate a previously presented session after successful authentication.
    if let Ok(previous) = session_token(&headers) {
        match state.identity.revoke_session(previous).await {
            Ok(()) | Err(IdentityError::InvalidSession) => {}
            Err(error) => return Err(identity_status(error)),
        }
    }
    let cookie = format!(
        "{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_LIFETIME_SECONDS}",
        token.as_str()
    );
    let mut response = Json(user).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn me(State(state): State<AuthState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    check_request(&state, &headers, false)?;
    let mut response = Json(principal(&state, &headers).await?).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn logout(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_request(&state, &headers, true)?;
    if let Ok(token) = session_token(&headers) {
        match state.identity.revoke_session(token).await {
            Ok(()) | Err(IdentityError::InvalidSession) => {}
            Err(error) => return Err(identity_status(error)),
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("panggung_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_budget_is_bounded() {
        let attempts = Arc::new(Mutex::new(VecDeque::new()));
        let state = AuthState {
            identity: IdentityStore::new(
                sqlx::sqlite::SqlitePoolOptions::new()
                    .connect_lazy("sqlite::memory:")
                    .unwrap(),
            ),
            authority: "127.0.0.1:3000".into(),
            origin: "http://127.0.0.1:3000".into(),
            attempts,
        };
        for _ in 0..LOGIN_LIMIT {
            assert!(state.admit_login().is_ok());
        }
        assert_eq!(state.admit_login(), Err(StatusCode::TOO_MANY_REQUESTS));
        state.attempts.lock().unwrap().clear();
        assert!(state.admit_login().is_ok());
    }
}
