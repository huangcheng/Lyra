//! HTTP auth route handlers.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};

use crate::crypto;
use crate::kv::KvStore;
use crate::storage::DbPool;
use uuid::Uuid;
use zeroize::Zeroizing;

use sea_orm::sea_query::{Expr, Query};
use sea_orm::{ColumnTrait, ConnectionTrait};

use super::db::{
    UserData, dberr_to_sqlx, find_first_user_totp_enabled, find_user_by_id, find_user_by_username,
    has_any_user, id_bind_value, insert_user, is_unique_violation, parse_mark_read_policy,
    update_user_password, update_user_totp, user_info_from,
};
use super::dek::{crypto_err, master_key};
use super::password::{hash_password, validate_password, verify_password};
use super::session::{
    clear_failed_attempts, ensure_not_rate_limited, invalidate_user_sessions, login_rl_key,
    note_failed_attempt, pwd_rl_key, totp_rl_key, totp_step_key,
};
use super::state::AuthState;
use super::totp::{
    build_totp, build_totp_from_raw, decrypt_totp_secret, encrypt_totp_secret, matched_totp_step,
};
use super::{
    AuthError, AuthStatus, AuthUser, BOOTSTRAP_TAKEN, BootstrapRequest, ChangePasswordRequest,
    LoginRequest, LoginResponse, PreferencesRequest, TOTP_LAST_STEP_TTL_SECS, TotpDisableRequest,
    TotpEnrollConfirmRequest, TotpEnrollResponse, TotpVerifyRequest, UserInfo,
    extract_token_from_headers,
};
use crate::entities::lyra_user as user_entity;

pub(super) async fn auth_status(State(state): State<AuthState>) -> Json<AuthStatus> {
    let has_user = has_any_user(&state.db).await.is_ok_and(|v| v);
    let totp_enabled = if has_user {
        find_first_user_totp_enabled(&state.db)
            .await
            .is_ok_and(|v| v)
    } else {
        false
    };
    Json(AuthStatus {
        has_user,
        totp_enabled,
    })
}

pub(super) async fn auth_bootstrap(
    State(state): State<AuthState>,
    Json(req): Json<BootstrapRequest>,
) -> Result<(StatusCode, Json<LoginResponse>), AuthError> {
    // Fast-path UX check; the real guard is the `singleton` unique index on
    // lyra_user (migration 0005), which rejects a second row even when two
    // concurrent bootstraps both pass this check.
    if has_any_user(&state.db).await.is_ok_and(|v| v) {
        return Err(AuthError::Conflict(BOOTSTRAP_TAKEN.to_string()));
    }

    if req.username.is_empty() || req.username.len() > 64 {
        return Err(AuthError::BadRequest(
            "Username must be between 1 and 64 characters".to_string(),
        ));
    }

    if let Err(msg) = validate_password(&req.password, state.min_password_length) {
        return Err(AuthError::BadRequest(msg));
    }

    let password_hash = hash_password(&req.password).await?;

    let user_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let locale = req.locale.unwrap_or_else(|| "en".to_string());

    // Generate the user's DEK and store it wrapped with the per-user KEK.
    let dek = crypto::generate_key();
    let kek = crypto::derive_user_kek(master_key().map_err(crypto_err)?, &user_id);
    let wrapped_dek = crypto::wrap_dek(&kek, &dek).map_err(crypto_err)?;

    insert_user(
        &state.db,
        &user_id,
        &req.username,
        &password_hash,
        req.display_name.as_deref(),
        &locale,
        &wrapped_dek,
    )
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            // Lost the bootstrap race (singleton guard) or username taken.
            AuthError::Conflict(BOOTSTRAP_TAKEN.to_string())
        } else {
            tracing::error!("Failed to insert user: {e}");
            AuthError::internal("Failed to create user")
        }
    })?;

    let token = state
        .sessions
        .create_session(&user_id)
        .await
        .map_err(|_| AuthError::internal("Failed to create session"))?;

    Ok((
        StatusCode::CREATED,
        Json(LoginResponse {
            token,
            user: UserInfo {
                id: user_id,
                username: req.username,
                display_name: req.display_name,
                locale,
                totp_enabled: false,
                mark_read_policy: "on_open".to_string(),
                ui_state: None,
            },
            requires_totp: false,
        }),
    ))
}

pub(super) async fn auth_login(
    State(state): State<AuthState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = login_rl_key(&req.username);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Authentication failed").await?;

    let Some(user) = find_user_by_username(&state.db, &req.username).await? else {
        note_failed_attempt(kv.as_ref(), &rl_key, "Authentication failed").await?;
        return Err(AuthError::invalid_credentials());
    };

    let valid = verify_password(
        &req.password,
        user.password_hash
            .as_deref()
            .ok_or_else(|| AuthError::internal("Password hash not available"))?,
    )
    .await
    .map_err(|_| AuthError::internal("Authentication failed"))?;

    if !valid {
        note_failed_attempt(kv.as_ref(), &rl_key, "Authentication failed").await?;
        return Err(AuthError::invalid_credentials());
    }

    // Password correct: reset the failed-attempt counter for this username.
    clear_failed_attempts(kv.as_ref(), &rl_key).await;

    if user.totp_enabled {
        let pending_token = state
            .sessions
            .create_pending_session(&user.id)
            .await
            .map_err(|_| AuthError::internal("Failed to create pending session"))?;
        return Ok(Json(LoginResponse {
            token: pending_token,
            user: user_info_from(&user),
            requires_totp: true,
        }));
    }

    let token = state
        .sessions
        .create_session(&user.id)
        .await
        .map_err(|_| AuthError::internal("Failed to create session"))?;

    Ok(Json(LoginResponse {
        token,
        user: user_info_from(&user),
        requires_totp: false,
    }))
}

pub(super) async fn totp_verify(
    State(state): State<AuthState>,
    Json(req): Json<TotpVerifyRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let kv = Arc::clone(state.sessions.kv());
    // Resolve the pending session first so the limiter keys on the user, not
    // the token — otherwise re-login would mint a fresh allowance.
    let user_id = state
        .sessions
        .get_pending_session(&req.pending_token)
        .await
        .ok_or_else(|| AuthError::unauthorized("Invalid or expired pending session"))?;

    let rl_key = totp_rl_key(&user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    verify_totp_code(&state, kv.as_ref(), &rl_key, &user_id, &user, &req.code).await?;

    let token = state
        .sessions
        .promote_pending_session(&req.pending_token)
        .await
        .map_err(|_| AuthError::internal("Failed to promote session"))?
        .ok_or_else(|| AuthError::unauthorized("Invalid or expired pending session"))?;

    Ok(Json(LoginResponse {
        token,
        user: user_info_from(&user),
        requires_totp: false,
    }))
}

/// Verify a login TOTP code for `user`, with per-user rate limiting and
/// replay protection (last accepted timestep in kv).
pub(super) async fn verify_totp_code(
    state: &AuthState,
    kv: &dyn KvStore,
    rl_key: &str,
    user_id: &str,
    user: &UserData,
    code: &str,
) -> Result<(), AuthError> {
    let stored_secret = user
        .totp_secret
        .as_deref()
        .ok_or_else(|| AuthError::internal("TOTP not configured"))?;
    let dek = AuthState::get_user_dek(&state.db, user_id)
        .await
        .map_err(crypto_err)?;
    let secret = Zeroizing::new(decrypt_totp_secret(&dek, stored_secret).map_err(crypto_err)?);
    let totp = build_totp(&secret, &user.username)?;

    let Some(step) = matched_totp_step(&totp, code) else {
        note_failed_attempt(kv, rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("Invalid TOTP code"));
    };

    // Replay guard: a code at or below the last accepted timestep was already
    // used (codes stay valid for ±1 step, so the ±1 skew allows reuse).
    let step_key = totp_step_key(user_id);
    let last_step: Option<u64> = kv
        .get(&step_key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    if last_step.is_some_and(|s| step <= s) {
        note_failed_attempt(kv, rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("TOTP code already used"));
    }
    let step_value = step.to_string();
    kv.set(&step_key, &step_value, Some(TOTP_LAST_STEP_TTL_SECS))
        .await
        .map_err(|e| {
            tracing::error!("totp step store failed: {e}");
            AuthError::internal("Verification failed")
        })?;
    clear_failed_attempts(kv, rl_key).await;
    Ok(())
}

pub(super) async fn totp_enroll(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<TotpEnrollResponse>, AuthError> {
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    // Re-enrolling while 2FA is active would silently rotate the secret;
    // the user must disable first (which requires the password).
    if user.totp_enabled {
        return Err(AuthError::Conflict(
            "TOTP is already enabled. Disable it before re-enrolling.".to_string(),
        ));
    }

    let secret_bytes =
        Zeroizing::new(totp_rs::Secret::generate_secret().to_bytes().map_err(|e| {
            tracing::error!("Failed to generate TOTP secret: {e}");
            AuthError::internal("Failed to generate TOTP secret")
        })?);

    let secret_base32 = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
    // Store the secret encrypted with the user's DEK; enabled only after confirm.
    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(crypto_err)?;
    let stored_secret = encrypt_totp_secret(&dek, &secret_base32).map_err(crypto_err)?;
    update_user_totp(&state.db, &user_id, Some(&stored_secret), false)
        .await
        .map_err(|_| AuthError::internal("Failed to store TOTP secret"))?;

    let totp = build_totp_from_raw(&secret_bytes, &user.username)?;

    Ok(Json(TotpEnrollResponse {
        secret: secret_base32,
        otpauth_uri: totp.get_url(),
    }))
}

pub(super) async fn totp_enroll_confirm(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<TotpEnrollConfirmRequest>,
) -> Result<Json<AuthStatus>, AuthError> {
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let stored = user
        .totp_secret
        .ok_or_else(|| AuthError::BadRequest("TOTP enrollment not started".to_string()))?;

    let dek = AuthState::get_user_dek(&state.db, &user_id)
        .await
        .map_err(crypto_err)?;
    let secret = Zeroizing::new(decrypt_totp_secret(&dek, &stored).map_err(crypto_err)?);

    let totp = build_totp(&secret, &user.username)?;

    if !totp.check_current(&req.code).unwrap_or(false) {
        return Err(AuthError::unauthorized(
            "Invalid TOTP code. Please try again.",
        ));
    }

    // Code verified: keep the same encrypted secret, flip the enabled flag.
    update_user_totp(&state.db, &user_id, Some(&stored), true)
        .await
        .map_err(|_| AuthError::internal("Failed to enable TOTP"))?;

    Ok(Json(AuthStatus {
        has_user: true,
        totp_enabled: true,
    }))
}

pub(super) async fn totp_disable(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<TotpDisableRequest>,
) -> Result<Json<AuthStatus>, AuthError> {
    // Disabling 2FA weakens account security, so it requires re-authentication
    // with the current password, not just a session token. The password check
    // is rate-limited per user so a stolen session is no password oracle.
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = pwd_rl_key(&user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let valid = verify_password(
        &req.password,
        &user
            .password_hash
            .ok_or_else(|| AuthError::internal("Password hash not available"))?,
    )
    .await
    .map_err(|_| AuthError::internal("Verification failed"))?;
    if !valid {
        note_failed_attempt(kv.as_ref(), &rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("Invalid password"));
    }
    clear_failed_attempts(kv.as_ref(), &rl_key).await;

    update_user_totp(&state.db, &user_id, None, false)
        .await
        .map_err(|_| AuthError::internal("Failed to disable TOTP"))?;

    Ok(Json(AuthStatus {
        has_user: true,
        totp_enabled: false,
    }))
}

pub(super) async fn change_password(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<StatusCode, AuthError> {
    // The current-password check is rate-limited per user so a stolen session
    // is no offline-speed password oracle.
    let kv = Arc::clone(state.sessions.kv());
    let rl_key = pwd_rl_key(&user_id);
    ensure_not_rate_limited(kv.as_ref(), &rl_key, "Verification failed").await?;

    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    let valid = verify_password(
        &req.current_password,
        &user
            .password_hash
            .ok_or_else(|| AuthError::internal("Password hash not available"))?,
    )
    .await
    .map_err(|_| AuthError::internal("Verification failed"))?;
    if !valid {
        note_failed_attempt(kv.as_ref(), &rl_key, "Verification failed").await?;
        return Err(AuthError::unauthorized("Current password is incorrect"));
    }
    clear_failed_attempts(kv.as_ref(), &rl_key).await;

    if let Err(msg) = validate_password(&req.new_password, state.min_password_length) {
        return Err(AuthError::BadRequest(msg));
    }

    let new_hash = hash_password(&req.new_password).await?;
    update_user_password(&state.db, &user_id, &new_hash)
        .await
        .map_err(|_| AuthError::internal("Failed to update password"))?;

    // Kick every session, including the caller's: after a password change all
    // clients must log in again with the new password. (Simplest and safest —
    // the caller's session is not special-cased.)
    invalidate_user_sessions(&state.db, state.sessions.kv().as_ref(), &user_id)
        .await
        .map_err(|_| AuthError::internal("Failed to invalidate sessions"))?;

    // Also clear any login lockout for this username, so the legit user is
    // not stuck behind an attacker's failed-attempt window.
    clear_failed_attempts(kv.as_ref(), &login_rl_key(&user.username)).await;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn auth_logout(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<StatusCode, AuthError> {
    if let Some(token) = extract_token_from_headers(&headers) {
        state.opengpg_unlock.lock(&token, None);
        state.sessions.remove_session(&token).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn auth_me(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<UserInfo>, AuthError> {
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;

    Ok(Json(user_info_from(&user)))
}

pub(super) async fn patch_preferences(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<PreferencesRequest>,
) -> Result<Json<UserInfo>, AuthError> {
    if req.mark_read_policy.is_none() && req.locale.is_none() && req.ui_state.is_none() {
        return Err(AuthError::BadRequest(
            "provide markReadPolicy, locale, and/or uiState".into(),
        ));
    }
    if let Some(raw) = req.mark_read_policy {
        let policy = parse_mark_read_policy(&raw)?;
        update_mark_read_policy(&state.db, &user_id, &policy).await?;
    }
    if let Some(locale) = req.locale {
        if !matches!(locale.as_str(), "en" | "zh") {
            return Err(AuthError::BadRequest("unsupported locale".into()));
        }
        update_locale(&state.db, &user_id, &locale).await?;
    }
    if let Some(ui_state) = req.ui_state {
        if !ui_state.is_object() {
            return Err(AuthError::BadRequest(
                "uiState must be a JSON object".into(),
            ));
        }
        let encoded = serde_json::to_string(&ui_state)
            .map_err(|_| AuthError::BadRequest("uiState is not serializable".into()))?;
        if encoded.len() > MAX_UI_STATE_BYTES {
            return Err(AuthError::BadRequest("uiState too large".into()));
        }
        update_ui_state(&state.db, &user_id, &encoded).await?;
    }
    let user = find_user_by_id(&state.db, &user_id)
        .await?
        .ok_or_else(|| AuthError::internal("User not found"))?;
    Ok(Json(user_info_from(&user)))
}

pub(super) async fn update_locale(
    db: &DbPool,
    user_id: &str,
    locale: &str,
) -> Result<(), AuthError> {
    let id =
        id_bind_value(db, user_id).map_err(|_| AuthError::internal("Failed to look up user"))?;
    let stmt = Query::update()
        .table(user_entity::Entity)
        .value(user_entity::Column::Locale, locale.to_string())
        .value(user_entity::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(user_entity::Column::Id.eq(id))
        .to_owned();
    db.orm().execute(&stmt).await.map_err(|e| {
        let e = dberr_to_sqlx(e);
        tracing::error!("DB error updating locale: {e}");
        AuthError::internal("Failed to update preferences")
    })?;
    Ok(())
}

/// Cap on the serialized UI view-state blob (view state is small by
/// definition; this stops abuse, not features).
const MAX_UI_STATE_BYTES: usize = 16 * 1024;

pub(super) async fn update_ui_state(
    db: &DbPool,
    user_id: &str,
    ui_state_json: &str,
) -> Result<(), AuthError> {
    let id =
        id_bind_value(db, user_id).map_err(|_| AuthError::internal("Failed to look up user"))?;
    let stmt = Query::update()
        .table(user_entity::Entity)
        .value(user_entity::Column::UiState, ui_state_json.to_string())
        .value(user_entity::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(user_entity::Column::Id.eq(id))
        .to_owned();
    db.orm().execute(&stmt).await.map_err(|e| {
        let e = dberr_to_sqlx(e);
        tracing::error!("DB error updating ui_state: {e}");
        AuthError::internal("Failed to update preferences")
    })?;
    Ok(())
}

pub(super) async fn update_mark_read_policy(
    db: &DbPool,
    user_id: &str,
    policy: &str,
) -> Result<(), AuthError> {
    let id =
        id_bind_value(db, user_id).map_err(|_| AuthError::internal("Failed to look up user"))?;
    let stmt = Query::update()
        .table(user_entity::Entity)
        .value(user_entity::Column::MarkReadPolicy, policy.to_string())
        .value(user_entity::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(user_entity::Column::Id.eq(id))
        .to_owned();
    db.orm().execute(&stmt).await.map_err(|e| {
        let e = dberr_to_sqlx(e);
        tracing::error!("DB error updating mark_read_policy: {e}");
        AuthError::internal("Failed to update preferences")
    })?;
    Ok(())
}
