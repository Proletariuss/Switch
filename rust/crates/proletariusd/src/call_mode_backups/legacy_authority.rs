//! Existing handler tests use genuine lifetime authority after body validation.
//! This compatibility surface is absent from production builds.
use super::*;

fn lease(root: &Path) -> crate::daemon_lifetime::Lease {
    crate::daemon_lifetime::Lease::acquire(root).expect("legacy fixture genuine root lease")
}

pub(super) async fn handle_internal(
    request: Request<Full<Bytes>>,
    root: &Path,
) -> Response<Full<Bytes>> {
    let body = request.body().clone().collect().await.unwrap().to_bytes();
    let held = (request.method() == Method::POST
        && request.uri().path() == INTERNAL_CREATE_PATH
        && created_backup_payload_id(&body).is_ok())
    .then(|| lease(root));
    let proof = held.as_ref().map(|held| held.root_proof()).ok_or(());
    handle_internal_with_proof(request, root, proof).await
}

pub(super) async fn handle_public(
    request: Request<Full<Bytes>>,
    root: &Path,
) -> Response<Full<Bytes>> {
    let body = request.body().clone().collect().await.unwrap().to_bytes();
    let held = (request.method() == Method::POST
        && request.uri().path() == "/api/call-mode/backups/import"
        && validate_import_payload(&body).is_ok())
    .then(|| lease(root));
    let proof = held.as_ref().map(|held| held.root_proof()).ok_or(());
    handle_public_with_proof(request, root, proof).await
}

pub(super) async fn handle_delete_public(
    request: Request<Full<Bytes>>,
    root: &Path,
    service: crate::module_radio::Service,
) -> Response<Full<Bytes>> {
    let body = request.body().clone().collect().await.unwrap().to_bytes();
    let eligible = serde_json::from_slice::<Option<DeleteRequest>>(&body)
        .ok()
        .flatten()
        .is_some_and(|parsed| {
            parsed.confirm == Some(true)
                && read_export(root, parsed.backup_id.as_deref().unwrap_or_default().trim()).is_ok()
        });
    let held = eligible.then(|| lease(root));
    let proof = held.as_ref().map(|held| held.root_proof()).ok_or(());
    handle_delete_public_with_proof(request, root, service, proof).await
}

pub(super) fn write_imported_backup(
    root: &Path,
    payload: &[u8],
    now: OffsetDateTime,
) -> Result<BackupSummary, ImportFailure> {
    validate_import_payload(payload).map_err(ImportFailure::BadRequest)?;
    let held = lease(root);
    write_imported_backup_with_proof(root, Ok(held.root_proof()), payload, now)
}
