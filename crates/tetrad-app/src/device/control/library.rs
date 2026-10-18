//! RecentsFavorites (20), RecentSearches (39) and PinnedModels (54) over the media library and
//! `zenui.db`.
//!
//! Stock answers a change with pushes from the list signals, not with a reply: they go out
//! inside the request, so they echo its ID (`0x1aca14`, `0x12bf58`).

use tetrad_control::codecs::{
    CortexMessageType, EnumValue, MessageAction, PinnedModelsMessage, RecentFavoriteType,
    RecentSearchesMessage, RecentsFavoritesItem, RecentsFavoritesMessage,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_library::stock::{ItemList, ItemRef, StockLibrary};
use tetrad_storage::media::Collection;

use super::super::{Effect, State};
use super::StateModel;

const FAVORITES: CortexMessageType = CortexMessageType::RecentsFavorites;
const SEARCHES: CortexMessageType = CortexMessageType::RecentSearches;
const PINNED: CortexMessageType = CortexMessageType::PinnedModels;
/// `0x7a5334`: an UPDATE with more searches is ignored (the device itself keeps 21).
const MAX_SEARCH_UPDATE: usize = 20;
/// `pinned_models.hash` text of at most 10 characters is a model id, longer is a capture key
/// (`0x495d08`, `0x495880`).
const MODEL_ID_CHARS: usize = 10;

fn refused(message_type: CortexMessageType, error: impl ToString) -> ModelError {
    ModelError::Refused(message_type, error.to_string())
}

fn library<'a>(
    model: &'a mut StateModel<'_>,
    message_type: CortexMessageType,
) -> DeviceResult<&'a mut StockLibrary> {
    (model.state.library.as_mut())
        .map(|library| &mut library.presets)
        .ok_or_else(|| refused(message_type, "no media library"))
}

/// [`library`] for a write.
fn writable<'a>(
    model: &'a mut StateModel<'_>,
    message_type: CortexMessageType,
) -> DeviceResult<&'a mut StockLibrary> {
    model.library_write(message_type)?;
    library(model, message_type)
}

/// Item types 0, 1 and 2 keep their value, any other type is 0 (`0x1aa02c`).
fn item_type(value: Option<EnumValue<RecentFavoriteType>>) -> RecentFavoriteType {
    match value {
        Some(EnumValue::Known(item_type)) => item_type,
        _ => RecentFavoriteType::Preset,
    }
}

fn collection(item_type: RecentFavoriteType) -> Collection {
    match item_type {
        RecentFavoriteType::Preset => Collection::Presets,
        RecentFavoriteType::ImpulseResponse => Collection::Irs,
        RecentFavoriteType::NeuralCapture => Collection::Captures,
    }
}

/// One list entry as `0x1aa4f0` sends it: the product name, folder key and name, the factory
/// flag (the factory preset folder only) and the product key. Inferred: the folder name is
/// the last part of the key (`0x705520`), a capture is named from the library listing and an
/// IR by its key; `is_plugin` (`vt+0x68`) is not modelled.
fn item(state: &State, item_type: RecentFavoriteType, entry: &ItemRef) -> RecentsFavoritesItem {
    let stem = |path: &str| {
        let file = path.rsplit('/').next().unwrap_or(path);
        file.strip_suffix(".pb").unwrap_or(file).to_owned()
    };
    let name = match item_type {
        RecentFavoriteType::Preset => stem(&entry.item),
        RecentFavoriteType::NeuralCapture => {
            (state.capture_names.get(&entry.item).cloned()).unwrap_or_else(|| entry.item.clone())
        }
        RecentFavoriteType::ImpulseResponse => entry.item.clone(),
    };
    let user = state
        .library
        .as_ref()
        .is_none_or(|library| library.presets.media().contains(&entry.folder));
    RecentsFavoritesItem {
        name: Some(name),
        folder_key: Some(entry.folder.clone()),
        folder_name: Some(stem(&entry.folder)),
        is_factory: Some(item_type == RecentFavoriteType::Preset && !user),
        is_plugin: None,
        product_key: Some(entry.item.clone()),
    }
}

/// The pushes of one list write, device or Control (the list signals `0x1abd00` removed,
/// `0x1aabe8` added, one item each): a DELETE for each entry that left `before` or that the
/// add replaced, then a CREATE for the added entry.
pub(in crate::device) fn list_pushes(
    state: &State,
    list: ItemList,
    before: &[ItemRef],
    after: &[ItemRef],
    added: Option<&ItemRef>,
) -> Vec<Effect> {
    let (favorites, collection) = match list {
        ItemList::Favorites(collection) => (true, collection),
        ItemList::Recents(collection) => (false, collection),
    };
    let item_type = match collection {
        Collection::Presets => RecentFavoriteType::Preset,
        Collection::Irs => RecentFavoriteType::ImpulseResponse,
        Collection::Captures => RecentFavoriteType::NeuralCapture,
    };
    let push = |action: MessageAction, entry: &ItemRef| {
        let message = RecentsFavoritesMessage {
            action: Some(action.into()),
            is_favorites: Some(favorites),
            items: vec![item(state, item_type, entry)],
            item_type: Some(item_type.into()),
            ..Default::default()
        };
        Effect::ControlNotify(Box::new(message.into()))
    };
    let removed = before
        .iter()
        .filter(|old| Some(*old) == added || !after.contains(old));
    let mut effects: Vec<Effect> = removed
        .map(|entry| push(MessageAction::Delete, entry))
        .collect();
    effects.extend(added.map(|entry| push(MessageAction::Create, entry)));
    effects
}

/// RecentsFavorites `0x1aa02c`. READ sends the list (`0x1ab5dc`, newest first). CREATE and
/// DELETE change favourites only: CREATE adds each item (a preset only when its file exists;
/// key `folder_key/name.pb`, `0x1a89a4`), DELETE removes every entry with the item's key
/// (`0x7cd32c`). The list signals push a DELETE for an entry the add replaces or the cap of
/// 64 drops, then the CREATE. Recents cannot be written. Inferred: several items take the
/// one-item path each (stock runs them as a bulk job, `0x7834b0`, `0x7835f0`).
pub(super) fn recents_favorites(
    model: &mut StateModel<'_>,
    request: &RecentsFavoritesMessage,
) -> DeviceResult<Option<RecentsFavoritesMessage>> {
    let item_type = item_type(request.item_type);
    let favorites = request.is_favorites.unwrap_or(false);
    let list = match favorites {
        true => ItemList::Favorites(collection(item_type)),
        false => ItemList::Recents(collection(item_type)),
    };
    let action = match request.action.unwrap_or(MessageAction::Create.into()) {
        EnumValue::Known(action) => action,
        EnumValue::Unknown(_) => return Ok(None),
    };
    match action {
        MessageAction::Read => {
            let entries = library(model, FAVORITES)?
                .items(list)
                .map_err(|error| refused(FAVORITES, error))?;
            let items = entries
                .iter()
                .map(|entry| item(model.state, item_type, entry))
                .collect();
            Ok(Some(RecentsFavoritesMessage {
                is_favorites: Some(favorites),
                items,
                item_type: Some(item_type.into()),
                ..Default::default()
            }))
        }
        MessageAction::Create if favorites => {
            for requested in &request.items {
                add_favorite(model, list, item_type, requested)?;
            }
            Ok(None)
        }
        MessageAction::Delete if favorites => {
            for requested in &request.items {
                remove_favorite(model, list, item_type, requested)?;
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// The stored entry of a requested item: presets by path, captures and IRs by product key.
fn entry(item_type: RecentFavoriteType, requested: &RecentsFavoritesItem) -> ItemRef {
    let folder = requested.folder_key.clone().unwrap_or_default();
    let item = match item_type {
        RecentFavoriteType::Preset => {
            format!(
                "{folder}/{}.pb",
                requested.name.as_deref().unwrap_or_default()
            )
        }
        _ => requested.product_key.clone().unwrap_or_default(),
    };
    ItemRef { folder, item }
}

fn add_favorite(
    model: &mut StateModel<'_>,
    list: ItemList,
    item_type: RecentFavoriteType,
    requested: &RecentsFavoritesItem,
) -> DeviceResult<()> {
    let entry = entry(item_type, requested);
    let library = writable(model, FAVORITES)?;
    if item_type == RecentFavoriteType::Preset && library.load(&entry.item).is_err() {
        return Ok(());
    }
    let before = library
        .items(list)
        .map_err(|error| refused(FAVORITES, error))?;
    library
        .push_item(list, &entry)
        .map_err(|error| refused(FAVORITES, error))?;
    let after = library
        .items(list)
        .map_err(|error| refused(FAVORITES, error))?;
    let pushes = list_pushes(model.state, list, &before, &after, Some(&entry));
    model.effects.extend(pushes);
    Ok(())
}

fn remove_favorite(
    model: &mut StateModel<'_>,
    list: ItemList,
    item_type: RecentFavoriteType,
    requested: &RecentsFavoritesItem,
) -> DeviceResult<()> {
    let key = entry(item_type, requested).item;
    let library = writable(model, FAVORITES)?;
    let before = (library.items(list)).map_err(|error| refused(FAVORITES, error))?;
    let matching: Vec<&ItemRef> = before.iter().filter(|entry| entry.item == key).collect();
    for entry in &matching {
        library
            .remove_item(list, entry)
            .map_err(|error| refused(FAVORITES, error))?;
    }
    let after: Vec<ItemRef> = (before.iter())
        .filter(|entry| entry.item != key)
        .cloned()
        .collect();
    let pushes = list_pushes(model.state, list, &before, &after, None);
    model.effects.extend(pushes);
    Ok(())
}

pub(super) fn recent_searches(model: &mut StateModel<'_>) -> DeviceResult<Vec<String>> {
    library(model, SEARCHES)?
        .recent_searches()
        .map_err(|error| refused(SEARCHES, error))
}

/// RecentSearches UPDATE `0x7a5334`: more than 20 queries are ignored; else the list is
/// replaced, written and pushed (`0x1a84d8`). Owned: an equal query given twice is kept once
/// (the library adds with the device rule).
pub(super) fn store_recent_searches(
    model: &mut StateModel<'_>,
    searches: &[String],
) -> DeviceResult<()> {
    if searches.len() > MAX_SEARCH_UPDATE {
        return Ok(());
    }
    let library = writable(model, SEARCHES)?;
    library
        .clear_recent_searches()
        .map_err(|error| refused(SEARCHES, error))?;
    for search in searches.iter().rev() {
        library
            .push_recent_search(search)
            .map_err(|error| refused(SEARCHES, error))?;
    }
    push_searches(model)
}

/// RecentSearches DELETE `0x7a50a0`: clear, write, push.
pub(super) fn clear_recent_searches(model: &mut StateModel<'_>) -> DeviceResult<()> {
    writable(model, SEARCHES)?
        .clear_recent_searches()
        .map_err(|error| refused(SEARCHES, error))?;
    push_searches(model)
}

/// `0x1a84d8`: UPDATE with every search, newest first.
fn push_searches(model: &mut StateModel<'_>) -> DeviceResult<()> {
    let searches = recent_searches(model)?;
    let message = RecentSearchesMessage {
        action: Some(MessageAction::Update.into()),
        searches,
        ..Default::default()
    };
    model
        .effects
        .push(Effect::ControlNotify(Box::new(message.into())));
    Ok(())
}

fn database<'a>(model: &'a StateModel<'_>) -> DeviceResult<&'a tetrad_storage::stock::StockDb> {
    (model.state.library.as_ref())
        .and_then(|library| library.database.as_ref())
        .ok_or_else(|| refused(PINNED, "zenui.db is not open"))
}

/// PinnedModels READ `0x12b2c8`: `pinned_models` newest first (rowid order reversed). Text of
/// at most 10 characters is a model id read with a signed `toInt` (an id at or above 2^31 is
/// dropped), longer text a capture key.
pub(super) fn pinned_models(model: &mut StateModel<'_>) -> DeviceResult<(Vec<u32>, Vec<String>)> {
    let mut pinned =
        (database(model)?.view().pinned_models()).map_err(|error| refused(PINNED, error))?;
    pinned.reverse();
    let (models, captures): (Vec<String>, Vec<String>) = pinned
        .into_iter()
        .partition(|hash| hash.chars().count() <= MODEL_ID_CHARS);
    let models = models
        .iter()
        .filter_map(|hash| hash.parse::<i32>().ok())
        .filter_map(|id| u32::try_from(id).ok())
        .collect();
    Ok((models, captures))
}

/// PinnedModels CREATE pins (`0x495494` models as decimal text, `0x495818` captures), DELETE
/// unpins (`0x49558c`, `0x495e98`). Each change pushes a one-item CREATE or DELETE
/// (`0x1304c8`, `0x1300a4`, `0x12f930`, `0x1308ec`). Owned: a model or capture already pinned
/// is not stored twice (stock inserts a copy). A `zenui.db` write: refused without `--persist`.
pub(super) fn pin_models(
    model: &mut StateModel<'_>,
    pin: bool,
    models: &[u32],
    captures: &[String],
) -> DeviceResult<()> {
    model.library_write(PINNED)?;
    let action = if pin {
        MessageAction::Create
    } else {
        MessageAction::Delete
    };
    let keys = (models.iter().map(|id| (id.to_string(), Some(*id), None))).chain(
        captures
            .iter()
            .map(|key| (key.clone(), None, Some(key.clone()))),
    );
    for (key, model_id, capture) in keys.collect::<Vec<_>>() {
        let db = (model.state.library.as_mut())
            .and_then(|library| library.database.as_mut())
            .ok_or_else(|| refused(PINNED, "zenui.db is not open"))?;
        db.transaction(|write| match pin {
            true => write.pin_model(&key),
            false => write.unpin_model(&key),
        })
        .map_err(|error| refused(PINNED, error))?;
        model.state.model_preset_revision += 1;
        let message = PinnedModelsMessage {
            action: Some(action.into()),
            models: model_id.into_iter().collect(),
            captures: capture.into_iter().collect(),
            ..Default::default()
        };
        model
            .effects
            .push(Effect::ControlNotify(Box::new(message.into())));
    }
    Ok(())
}
