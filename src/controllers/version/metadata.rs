//! Endpoints that expose metadata about crate versions
//!
//! These endpoints provide data that could be obtained directly from the
//! index or cached metadata which was extracted (client side) from the
//! `Cargo.toml` file.

use axum::extract::Path;
use axum::Json;
use axum_extra::json;
use axum_extra::response::ErasedJson;
use crates_io_database::schema::{crates, dependencies};
use crates_io_worker::BackgroundJob;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use http::request::Parts;
use http::StatusCode;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::{AuthCheck, Authentication};
use crate::models::token::EndpointScope;
use crate::models::{
    Crate, Dependency, NewVersionOwnerAction, Rights, Version, VersionAction, VersionOwnerAction,
};
use crate::rate_limiter::LimitedAction;
use crate::schema::versions;
use crate::util::errors::{bad_request, custom, version_not_found, AppResult};
use crate::views::{EncodableDependency, EncodableVersion};
use crate::worker::jobs::{SyncToGitIndex, SyncToSparseIndex, UpdateDefaultVersion};

use super::version_and_crate;

#[derive(Deserialize)]
pub struct VersionUpdate {
    yanked: Option<bool>,
    yank_message: Option<String>,
}
#[derive(Deserialize)]
pub struct VersionUpdateRequest {
    version: VersionUpdate,
}

/// Handles the `GET /crates/:crate_id/:version/dependencies` route.
///
/// This information can be obtained directly from the index.
///
/// In addition to returning cached data from the index, this returns
/// fields for `id`, `version_id`, and `downloads` (which appears to always
/// be 0)
pub async fn dependencies(
    state: AppState,
    Path((crate_name, version)): Path<(String, String)>,
) -> AppResult<ErasedJson> {
    if semver::Version::parse(&version).is_err() {
        return Err(version_not_found(&crate_name, &version));
    }

    let mut conn = state.db_read().await?;
    let (version, _) = version_and_crate(&mut conn, &crate_name, &version).await?;

    let deps = Dependency::belonging_to(&version)
        .inner_join(crates::table)
        .select((Dependency::as_select(), crates::name))
        .order((dependencies::optional, crates::name))
        .load::<(Dependency, String)>(&mut conn)
        .await?
        .into_iter()
        .map(|(dep, crate_name)| EncodableDependency::from_dep(dep, &crate_name))
        .collect::<Vec<_>>();

    Ok(json!({ "dependencies": deps }))
}

/// Handles the `GET /crates/:crate_id/:version/authors` route.
pub async fn authors() -> ErasedJson {
    // Currently we return the empty list.
    // Because the API is not used anymore after RFC https://github.com/rust-lang/rfcs/pull/3052.

    json!({
        "users": [],
        "meta": { "names": [] },
    })
}

/// Handles the `GET /crates/:crate/:version` route.
///
/// The frontend doesn't appear to hit this endpoint, but our tests do, and it seems to be a useful
/// API route to have.
pub async fn show(
    state: AppState,
    Path((crate_name, version)): Path<(String, String)>,
) -> AppResult<ErasedJson> {
    if semver::Version::parse(&version).is_err() {
        return Err(version_not_found(&crate_name, &version));
    }

    let mut conn = state.db_read().await?;
    let (version, krate) = version_and_crate(&mut conn, &crate_name, &version).await?;
    let published_by = version.published_by(&mut conn).await?;
    let actions = VersionOwnerAction::by_version(&mut conn, &version).await?;

    let version = EncodableVersion::from(version, &krate.name, published_by, actions);
    Ok(json!({ "version": version }))
}

/// Handles the `PATCH /crates/:crate/:version` route.
///
/// This endpoint allows updating the yanked state of a version, including a yank message.
pub async fn update(
    state: AppState,
    Path((crate_name, version)): Path<(String, String)>,
    req: Parts,
    Json(update_request): Json<VersionUpdateRequest>,
) -> AppResult<ErasedJson> {
    if semver::Version::parse(&version).is_err() {
        return Err(version_not_found(&crate_name, &version));
    }

    let mut conn = state.db_write().await?;
    let (mut version, krate) = version_and_crate(&mut conn, &crate_name, &version).await?;
    validate_yank_update(&update_request.version, &version)?;
    let auth = authenticate(&req, &mut conn, &krate.name).await?;

    state
        .rate_limiter
        .check_rate_limit(auth.user_id(), LimitedAction::YankUnyank, &mut conn)
        .await?;

    perform_version_yank_update(
        &state,
        &mut conn,
        &mut version,
        &krate,
        &auth,
        update_request.version.yanked,
        update_request.version.yank_message,
    )
    .await?;

    let published_by = version.published_by(&mut conn).await?;
    let actions = VersionOwnerAction::by_version(&mut conn, &version).await?;
    let updated_version = EncodableVersion::from(version, &krate.name, published_by, actions);
    Ok(json!({ "version": updated_version }))
}

fn validate_yank_update(update_data: &VersionUpdate, version: &Version) -> AppResult<()> {
    if update_data.yank_message.is_some() {
        if matches!(update_data.yanked, Some(false)) {
            return Err(bad_request("Cannot set yank message when unyanking"));
        }

        if update_data.yanked.is_none() && !version.yanked {
            return Err(bad_request(
                "Cannot update yank message for a version that is not yanked",
            ));
        }
    }

    Ok(())
}

pub async fn authenticate(
    req: &Parts,
    conn: &mut AsyncPgConnection,
    name: &str,
) -> AppResult<Authentication> {
    AuthCheck::default()
        .with_endpoint_scope(EndpointScope::Yank)
        .for_crate(name)
        .check(req, conn)
        .await
}

pub async fn perform_version_yank_update(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    version: &mut Version,
    krate: &Crate,
    auth: &Authentication,
    yanked: Option<bool>,
    yank_message: Option<String>,
) -> AppResult<()> {
    let api_token_id = auth.api_token_id();
    let user = auth.user();
    let owners = krate.owners(conn).await?;

    let yanked = yanked.unwrap_or(version.yanked);

    if user.rights(state, &owners).await? < Rights::Publish {
        if user.is_admin {
            let action = if yanked { "yanking" } else { "unyanking" };
            warn!(
                "Admin {} is {action} {}@{}",
                user.gh_login, krate.name, version.num
            );
        } else {
            return Err(custom(
                StatusCode::FORBIDDEN,
                "must already be an owner to yank or unyank",
            ));
        }
    }

    // Check if the yanked state or yank message has changed and update if necessary
    let updated_cnt = diesel::update(
        versions::table.find(version.id).filter(
            versions::yanked
                .is_distinct_from(yanked)
                .or(versions::yank_message.is_distinct_from(&yank_message)),
        ),
    )
    .set((
        versions::yanked.eq(yanked),
        versions::yank_message.eq(&yank_message),
    ))
    .execute(conn)
    .await?;

    // If no rows were updated, return early
    if updated_cnt == 0 {
        return Ok(());
    }

    // Apply the update to the version
    version.yanked = yanked;
    version.yank_message = yank_message;

    let action = if yanked {
        VersionAction::Yank
    } else {
        VersionAction::Unyank
    };
    NewVersionOwnerAction::builder()
        .version_id(version.id)
        .user_id(user.id)
        .maybe_api_token_id(api_token_id)
        .action(action)
        .build()
        .insert(conn)
        .await?;

    let git_index_job = SyncToGitIndex::new(&krate.name);
    let sparse_index_job = SyncToSparseIndex::new(&krate.name);
    let update_default_version_job = UpdateDefaultVersion::new(krate.id);

    tokio::try_join!(
        git_index_job.enqueue(conn),
        sparse_index_job.enqueue(conn),
        update_default_version_job.enqueue(conn),
    )?;

    Ok(())
}
