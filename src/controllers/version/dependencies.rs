use super::CrateVersionPath;
use crate::app::AppState;
use crate::models::Dependency;
use crate::util::errors::AppResult;
use crate::views::EncodableDependency;
use axum_extra::json;
use axum_extra::response::ErasedJson;
use crates_io_database::schema::{crates, dependencies};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Get crate version dependencies.
///
/// This information can also be obtained directly from the index.
///
/// In addition to returning cached data from the index, this returns
/// fields for `id`, `version_id`, and `downloads` (which appears to always
/// be 0)
#[utoipa::path(
    get,
    path = "/api/v1/crates/{name}/{version}/dependencies",
    params(CrateVersionPath),
    tag = "versions",
    responses((status = 200, description = "Successful Response")),
)]
pub async fn get_version_dependencies(
    state: AppState,
    path: CrateVersionPath,
) -> AppResult<ErasedJson> {
    let mut conn = state.db_read().await?;
    let version = path.load_version(&mut conn).await?;

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