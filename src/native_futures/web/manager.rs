//! Domestic data for the Manager browser API.
use super::*;
use crate::native_futures::config::Source;
use axum::extract::Query;
use serde::Deserialize;
use std::collections::BTreeSet;

mod catalog;
mod cost;
mod dashboard;
mod timeline;

pub(super) use catalog::{Scope, persist_position, persist_position_grants, position_access_view};
pub(super) use dashboard::{Observations, start_observing};

pub(super) fn routes() -> Router<Service> {
    Router::new()
        .route("/api/health", get(dashboard::health))
        .route("/api/account-overview", get(dashboard::overview))
        .route("/api/account-overview/nav", get(timeline::nav_monitor))
        .route("/api/dashboard", get(dashboard::dashboard))
        .route("/api/timeline", get(timeline::strategy))
        .route("/api/account-timeline", get(timeline::account))
        .route("/api/catalog/acquisition-cost", get(cost::report))
        .route("/api/catalog/kline-status", get(cost::kline_status))
        .merge(catalog::routes())
}

fn visible<'a>(s: &'a Service, access: &'a SourceAccess) -> impl Iterator<Item = &'a Source> {
    s.config
        .sources
        .iter()
        .filter(|source| source.enabled && access.view.contains(&source.id))
}

fn get_source<'a>(
    s: &'a Service,
    access: &'a SourceAccess,
    id: &str,
) -> Result<&'a Source, ApiError> {
    visible(s, access)
        .find(|source| source.id == id)
        .ok_or_else(forbidden)
}

fn selected(
    s: &Service,
    access: &SourceAccess,
    ids: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    let Some(ids) = ids else {
        return Ok(visible(s, access).map(|source| source.id.clone()).collect());
    };
    let mut result = BTreeSet::new();
    for id in ids.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        result.insert(get_source(s, access, id)?.id.clone());
    }
    if result.is_empty() {
        return Err(bad("请选择账户"));
    }
    Ok(result.into_iter().collect())
}
