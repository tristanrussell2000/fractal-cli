use super::{
    client::SapClient,
    edit_session::{
        AdtEditSessionError, AdtObjectLock, acquire_adt_object_lock,
        read_adt_source_in_stateful_session, release_adt_object_lock, write_adt_source,
    },
    editable_source::{
        AdtSourceReadError, AdtSourceSnapshot, AdtSourceVersion, EditableAdtSourceIdentity,
        ValidatedAdtEditTarget, read_adt_source_by_identity,
    },
};

/// A proposed inactive source plus metadata meaningful to its planner.
pub(super) struct PlannedInactiveSourceChange<M> {
    pub(super) proposed: AdtSourceSnapshot,
    pub(super) metadata: M,
}

/// Source snapshots captured around one successful guarded inactive-source save.
pub(super) struct SavedInactiveSourceChange<M> {
    pub(super) original: AdtSourceSnapshot,
    pub(super) proposed: AdtSourceSnapshot,
    pub(super) stored: AdtSourceSnapshot,
    pub(super) metadata: M,
}

/// Internal save stages mapped into each public workflow's contextual error.
pub(super) enum InactiveSourceSaveError<E> {
    Session(AdtEditSessionError),
    LockedSourceRead(AdtSourceReadError),
    Plan(E),
    StoredSourceRead(AdtSourceReadError),
}

/// Saves one planned inactive source through a lock/read/write/unlock/verify cycle.
///
/// The planner is synchronous because it operates only on the exact source snapshot
/// read under the lock. If both planning or writing and unlock fail, the original
/// operation error wins, while unlock is still attempted.
pub(super) async fn save_inactive_source_atomically<M, E, F>(
    sap: &mut SapClient,
    target: &ValidatedAdtEditTarget,
    planner: F,
) -> Result<SavedInactiveSourceChange<M>, InactiveSourceSaveError<E>>
where
    F: FnOnce(&AdtSourceSnapshot) -> Result<PlannedInactiveSourceChange<M>, E>,
{
    let identity = &target.identity;
    let lock = acquire_adt_object_lock(sap, &identity.object_uri, target.transport.as_deref())
        .await
        .map_err(InactiveSourceSaveError::Session)?;
    let operation = plan_and_write_while_locked(sap, identity, &lock, planner).await;
    let unlock = release_adt_object_lock(sap, &identity.object_uri, &lock).await;

    let (original, change) = match operation {
        Err(primary) => {
            // Cleanup errors must not replace the planning/read/write failure,
            // but releasing the lock was still attempted above.
            let _ = unlock;
            return Err(primary);
        }
        Ok(value) => {
            unlock.map_err(InactiveSourceSaveError::Session)?;
            value
        }
    };

    let stored = read_adt_source_by_identity(sap, identity, AdtSourceVersion::Inactive)
        .await
        .map(|read| read.snapshot)
        .map_err(InactiveSourceSaveError::StoredSourceRead)?;

    Ok(SavedInactiveSourceChange {
        original,
        proposed: change.proposed,
        stored,
        metadata: change.metadata,
    })
}

async fn plan_and_write_while_locked<M, E, F>(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
    planner: F,
) -> Result<(AdtSourceSnapshot, PlannedInactiveSourceChange<M>), InactiveSourceSaveError<E>>
where
    F: FnOnce(&AdtSourceSnapshot) -> Result<PlannedInactiveSourceChange<M>, E>,
{
    let original = read_adt_source_in_stateful_session(sap, identity, AdtSourceVersion::Inactive)
        .await
        .map(|read| read.snapshot)
        .map_err(InactiveSourceSaveError::LockedSourceRead)?;
    let change = planner(&original).map_err(InactiveSourceSaveError::Plan)?;
    write_adt_source(sap, identity, lock, &change.proposed.source)
        .await
        .map_err(InactiveSourceSaveError::Session)?;
    Ok((original, change))
}
