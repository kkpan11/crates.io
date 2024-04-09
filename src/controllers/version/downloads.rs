//! Functionality for downloading crates and maintaining download counts
//!
//! Crate level functionality is located in `krate::downloads`.

use super::version_and_crate;
use crate::controllers::prelude::*;
use crate::models::VersionDownload;
use crate::schema::*;
use crate::util::errors::version_not_found;
use crate::views::EncodableVersionDownload;
use chrono::{Duration, NaiveDate, Utc};

/// Handles the `GET /crates/:crate_id/:version/download` route.
/// This returns a URL to the location where the crate is stored.
pub async fn download(
    app: AppState,
    Path((crate_name, version)): Path<(String, String)>,
    req: Parts,
) -> AppResult<Response> {
    let wants_json = req.wants_json();
    let redirect_url = app.storage.crate_location(&crate_name, &version);
    if wants_json {
        Ok(Json(json!({ "url": redirect_url })).into_response())
    } else {
        Ok(redirect(redirect_url))
    }
}

/// Handles the `GET /crates/:crate_id/:version/downloads` route.
pub async fn downloads(
    app: AppState,
    Path((crate_name, version)): Path<(String, String)>,
    req: Parts,
) -> AppResult<Json<Value>> {
    if semver::Version::parse(&version).is_err() {
        return Err(version_not_found(&crate_name, &version));
    }

    let conn = app.db_read_async().await?;
    conn.interact(move |conn| {
        let (version, _) = version_and_crate(conn, &crate_name, &version)?;

        let cutoff_end_date = req
            .query()
            .get("before_date")
            .and_then(|d| NaiveDate::parse_from_str(d, "%F").ok())
            .unwrap_or_else(|| Utc::now().date_naive());
        let cutoff_start_date = cutoff_end_date - Duration::days(89);

        let downloads = VersionDownload::belonging_to(&version)
            .filter(version_downloads::date.between(cutoff_start_date, cutoff_end_date))
            .order(version_downloads::date)
            .load(conn)?
            .into_iter()
            .map(VersionDownload::into)
            .collect::<Vec<EncodableVersionDownload>>();

        Ok(Json(json!({ "version_downloads": downloads })))
    })
    .await?
}
