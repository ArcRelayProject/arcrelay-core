use super::*;

fn boundary(model: &clipboard_entry::Model, sort_by: ClipboardSortBy) -> ClipboardCursor {
    ClipboardCursor {
        sort_at_ms: sort_timestamp(model, sort_by),
        id: model.id.max(0) as u64,
    }
}

fn timeline_condition(cursor: ClipboardCursor, sort_by: ClipboardSortBy, newer: bool) -> Condition {
    if !newer {
        return cursor_condition(cursor, sort_by);
    }
    let id = i64::try_from(cursor.id).unwrap_or(i64::MAX);
    let time = sort_expression(sort_by);
    Condition::any()
        .add(time.clone().gt(cursor.sort_at_ms))
        .add(
            Condition::all()
                .add(time.eq(cursor.sort_at_ms))
                .add(Expr::col(clipboard_entry::Column::SyncId).gt(cursor_sync_id(id))),
        )
}

async fn side<C: ConnectionTrait>(
    db: &C,
    cursor: ClipboardCursor,
    sort_by: ClipboardSortBy,
    newer: bool,
    limit: usize,
) -> Result<Vec<clipboard_entry::Model>, DbErr> {
    // Ascending selects the *nearest* newer rows, not the newest page in the
    // entire database. Reverse them for the common newest-first presentation.
    let order = if newer { Order::Asc } else { Order::Desc };
    let mut models = summary_query()
        .filter(clipboard_entry::Column::Deleted.eq(false))
        .filter(timeline_condition(cursor, sort_by, newer))
        .order_by(sort_expression(sort_by), order.clone())
        .order_by(clipboard_entry::Column::SyncId, order)
        .limit(limit as u64)
        .all(db)
        .await?;
    if newer {
        models.reverse();
    }
    Ok(models)
}

async fn continuation<C: ConnectionTrait>(
    db: &C,
    model: Option<&clipboard_entry::Model>,
    sort_by: ClipboardSortBy,
    newer: bool,
) -> Result<Option<ClipboardCursor>, DbErr> {
    let Some(model) = model else { return Ok(None) };
    let cursor = boundary(model, sort_by);
    let exists = clipboard_entry::Entity::find()
        .select_only()
        .column(clipboard_entry::Column::Id)
        .filter(clipboard_entry::Column::Deleted.eq(false))
        .filter(timeline_condition(cursor, sort_by, newer))
        .into_tuple::<i64>()
        .one(db)
        .await?
        .is_some();
    Ok(exists.then_some(cursor))
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn timeline(
        &self,
        query: ClipboardTimelineQuery,
    ) -> Result<Option<ClipboardTimelinePage>, DbErr> {
        let limit = query.limit.clamp(1, 120);
        // Anchor, neighbors, metadata and revision share one read transaction.
        // Do not use payload(): that records a use and changes history order.
        let db = self.db.begin().await?;
        let (anchor, models) = match query.position {
            ClipboardTimelinePosition::AroundId(id) => {
                let id =
                    i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
                let Some(model) = summary_query()
                    .filter(clipboard_entry::Column::Id.eq(id))
                    .filter(clipboard_entry::Column::Deleted.eq(false))
                    .one(&db)
                    .await?
                else {
                    db.commit().await?;
                    return Ok(None);
                };
                let cursor = boundary(&model, query.sort_by);
                let mut models = side(&db, cursor, query.sort_by, true, limit).await?;
                models.push(model);
                models.extend(side(&db, cursor, query.sort_by, false, limit).await?);
                (Some(cursor), models)
            }
            ClipboardTimelinePosition::NewerThan(cursor) => {
                (None, side(&db, cursor, query.sort_by, true, limit).await?)
            }
            ClipboardTimelinePosition::OlderThan(cursor) => {
                (None, side(&db, cursor, query.sort_by, false, limit).await?)
            }
        };
        let newer_cursor = continuation(&db, models.first(), query.sort_by, true).await?;
        let older_cursor = continuation(&db, models.last(), query.sort_by, false).await?;
        let total_count = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .count(&db)
            .await?;
        let revision = clipboard_state::Entity::find_by_id(STATE_ID)
            .one(&db)
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))?
            .revision
            .max(0) as u64;
        let sync_ids = models
            .iter()
            .map(|model| model.sync_id.clone())
            .collect::<Vec<_>>();
        let mut labels = self.active_labels_for_sync_ids_on(&db, &sync_ids).await?;
        let entries = models
            .into_iter()
            .map(|model| {
                let labels = labels.remove(&model.sync_id).unwrap_or_default();
                model_to_summary(model, labels)
            })
            .collect::<Result<Vec<_>, _>>()?;
        db.commit().await?;
        Ok(Some(ClipboardTimelinePage {
            revision,
            entries,
            anchor,
            newer_cursor,
            older_cursor,
            total_count,
        }))
    }
}
