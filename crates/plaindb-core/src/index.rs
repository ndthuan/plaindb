use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use roaring::RoaringBitmap;
use rpds::{RedBlackTreeMapSync, VectorSync};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec::Doc;
use crate::config::{CollectionMeta, Format, IndexSpec};
use crate::error::{Error, ErrorCode, Result};
use crate::query::{Key, split_path};
use crate::query::{crosses_array, elements, terminals};
use crate::snapshot::{CollectionSnapshot, FileEntry, Outcome};
use crate::text::{TextFields, doc_tokens, tokens};

pub type PendingWrite<'a> = (&'a str, Option<&'a Doc>, usize);
pub type Postings = Arc<RoaringBitmap>;
type PostingMap<K> = RedBlackTreeMapSync<K, Postings>;

#[derive(Serialize, Deserialize)]
pub struct FieldDump {
    pub entries: Vec<(Value, Vec<u32>)>,
    pub unindexed: Vec<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct IndexDump {
    pub fields: Vec<FieldDump>,
    pub text: Option<Vec<(String, Vec<u32>)>>,
}

#[derive(Debug, Clone)]
pub struct Ordinals {
    by_id: RedBlackTreeMapSync<Arc<str>, u32>,
    ids: VectorSync<Option<Arc<str>>>,
    free: VectorSync<u32>,
}

impl Default for Ordinals {
    fn default() -> Self {
        Self {
            by_id: RedBlackTreeMapSync::new_sync(),
            ids: VectorSync::new_sync(),
            free: VectorSync::new_sync(),
        }
    }
}

impl Ordinals {
    fn listed(ids: &[&str]) -> Self {
        let mut ordinals = Self::default();
        for id in ids {
            ordinals.assign(id);
        }
        ordinals
    }

    pub fn of(&self, id: &str) -> Option<u32> {
        self.by_id.get(id).copied()
    }

    pub fn id(&self, ordinal: u32) -> Option<&str> {
        self.ids.get(usize::try_from(ordinal).ok()?)?.as_deref()
    }

    pub fn ids(&self, set: &RoaringBitmap) -> Vec<String> {
        set.iter()
            .filter_map(|ordinal| self.id(ordinal).map(str::to_owned))
            .collect()
    }

    fn assign(&mut self, id: &str) -> u32 {
        if let Some(known) = self.of(id) {
            return known;
        }
        let shared: Arc<str> = Arc::from(id);
        let ordinal = match self.free.last().copied() {
            Some(reused) => {
                self.free.drop_last_mut();
                self.ids.set_mut(reused as usize, Some(Arc::clone(&shared)));
                reused
            }
            None => {
                let next = u32::try_from(self.ids.len()).unwrap_or(u32::MAX);
                self.ids.push_back_mut(Some(Arc::clone(&shared)));
                next
            }
        };
        self.by_id.insert_mut(shared, ordinal);
        ordinal
    }

    fn release(&mut self, id: &str) {
        let Some(ordinal) = self.of(id) else {
            return;
        };
        self.by_id.remove_mut(id);
        self.ids.set_mut(ordinal as usize, None);
        self.free.push_back_mut(ordinal);
    }
}

fn add_posting<K: Ord + Clone>(map: &mut PostingMap<K>, key: K, ordinal: u32) {
    match map.get_mut(&key) {
        Some(set) => {
            Arc::make_mut(set).insert(ordinal);
        }
        None => map.insert_mut(key, Arc::new(RoaringBitmap::from_iter([ordinal]))),
    }
}

fn remove_posting<K: Ord + Clone>(map: &mut PostingMap<K>, key: &K, ordinal: u32) {
    let emptied = match map.get(key) {
        Some(set) if set.contains(ordinal) => set.len() == 1,
        _ => return,
    };
    if emptied {
        map.remove_mut(key);
    } else if let Some(set) = map.get_mut(key) {
        Arc::make_mut(set).remove(ordinal);
    }
}

fn postings(ordinals: &[u32]) -> Postings {
    Arc::new(ordinals.iter().copied().collect())
}

fn positions(set: &RoaringBitmap, ordinals: &Ordinals, lookup: &HashMap<&str, u32>) -> Vec<u32> {
    set.iter()
        .filter_map(|ordinal| ordinals.id(ordinal))
        .filter_map(|id| lookup.get(id).copied())
        .collect()
}

fn restored(positions: &[u32], count: usize) -> Option<Postings> {
    let limit = u32::try_from(count).ok()?;
    positions
        .iter()
        .all(|position| *position < limit)
        .then(|| postings(positions))
}

#[derive(Debug, Clone)]
pub struct FieldIndex {
    pub field: String,
    pub path: Vec<String>,
    pub unique: bool,
    pub multi: bool,
    entries: PostingMap<Key>,
    unindexed: Postings,
}

pub enum Keys {
    Some(Vec<Value>),
    Refused,
}

impl FieldIndex {
    fn new(field: &str, unique: bool, multi: bool) -> Result<Self> {
        Ok(Self {
            field: field.to_owned(),
            path: split_path(field).map_err(|_| invalid_index(field))?,
            unique,
            multi,
            entries: RedBlackTreeMapSync::new_sync(),
            unindexed: Postings::default(),
        })
    }

    pub fn keys_for(&self, doc: &Doc) -> Keys {
        if self.multi {
            return Keys::Some(elements(doc, &self.path).into_iter().cloned().collect());
        }
        let found = terminals(doc, &self.path);
        if crosses_array(doc, &self.path) || found.iter().any(|value| value.is_array()) {
            return Keys::Refused;
        }
        Keys::Some(found.into_iter().cloned().collect())
    }

    fn add(&mut self, ordinal: u32, doc: &Doc) {
        match self.keys_for(doc) {
            Keys::Refused => {
                Arc::make_mut(&mut self.unindexed).insert(ordinal);
            }
            Keys::Some(keys) => {
                for key in keys {
                    add_posting(&mut self.entries, Key(key), ordinal);
                }
            }
        }
    }

    fn bulk(&mut self, docs: &[(u32, &Doc)]) {
        let mut grouped: BTreeMap<Key, Vec<u32>> = BTreeMap::new();
        let mut unindexed = Vec::new();
        for (ordinal, doc) in docs {
            match self.keys_for(doc) {
                Keys::Refused => unindexed.push(*ordinal),
                Keys::Some(keys) => {
                    for key in keys {
                        grouped.entry(Key(key)).or_default().push(*ordinal);
                    }
                }
            }
        }
        for (key, ordinals) in grouped {
            self.entries.insert_mut(key, postings(&ordinals));
        }
        self.unindexed = postings(&unindexed);
    }

    fn remove(&mut self, ordinal: u32, doc: &Doc) {
        match self.keys_for(doc) {
            Keys::Refused => {
                if self.unindexed.contains(ordinal) {
                    Arc::make_mut(&mut self.unindexed).remove(ordinal);
                }
            }
            Keys::Some(keys) => {
                for key in keys {
                    remove_posting(&mut self.entries, &Key(key), ordinal);
                }
            }
        }
    }

    pub fn exact(&self, value: &Value) -> RoaringBitmap {
        self.entries
            .get(&Key(value.clone()))
            .map(|set| RoaringBitmap::clone(set))
            .unwrap_or_default()
    }

    pub fn range(&self, from: Bound<Value>, to: Bound<Value>) -> RoaringBitmap {
        let wrap = |bound: Bound<Value>| match bound {
            Bound::Included(value) => Bound::Included(Key(value)),
            Bound::Excluded(value) => Bound::Excluded(Key(value)),
            Bound::Unbounded => Bound::Unbounded,
        };
        let mut found = RoaringBitmap::new();
        for (_, set) in self.entries.range((wrap(from), wrap(to))) {
            found |= set.as_ref();
        }
        found
    }

    pub fn prefix(&self, prefix: &str) -> RoaringBitmap {
        let mut found = RoaringBitmap::new();
        for (_, set) in self
            .entries
            .range((
                Bound::Included(Key(Value::String(prefix.to_owned()))),
                Bound::Unbounded,
            ))
            .take_while(
                |(key, _)| matches!(&key.0, Value::String(text) if text.starts_with(prefix)),
            )
        {
            found |= set.as_ref();
        }
        found
    }

    pub fn unindexed(&self) -> &RoaringBitmap {
        &self.unindexed
    }

    fn duplicated(&self) -> impl Iterator<Item = (&Value, &RoaringBitmap)> {
        self.entries
            .iter()
            .filter(|(_, set)| set.len() > 1)
            .map(|(key, set)| (&key.0, set.as_ref()))
    }

    fn duplicates(&self) -> Option<(&Value, &RoaringBitmap)> {
        self.duplicated().next()
    }
}

#[derive(Debug, Clone)]
pub struct TextIndex {
    pub fields: TextFields,
    entries: PostingMap<String>,
}

impl TextIndex {
    fn bulk(&mut self, docs: &[(u32, &Doc)]) {
        let mut grouped: HashMap<String, Vec<u32>> = HashMap::new();
        for (ordinal, doc) in docs {
            for token in doc_tokens(doc, &self.fields) {
                grouped.entry(token).or_default().push(*ordinal);
            }
        }
        for (token, ordinals) in grouped {
            self.entries.insert_mut(token, postings(&ordinals));
        }
    }

    fn replace(&mut self, ordinal: u32, old: Option<&Doc>, new: Option<&Doc>) {
        let before = old
            .map(|doc| doc_tokens(doc, &self.fields))
            .unwrap_or_default();
        let after = new
            .map(|doc| doc_tokens(doc, &self.fields))
            .unwrap_or_default();
        for token in before.difference(&after) {
            remove_posting(&mut self.entries, token, ordinal);
        }
        for token in after.difference(&before) {
            add_posting(&mut self.entries, token.clone(), ordinal);
        }
    }

    pub fn matching(&self, query: &str) -> RoaringBitmap {
        let mut found: Option<RoaringBitmap> = None;
        for token in tokens(query) {
            let Some(set) = self.entries.get(&token) else {
                return RoaringBitmap::new();
            };
            found = Some(match found {
                Some(known) => known & set.as_ref(),
                None => RoaringBitmap::clone(set),
            });
        }
        found.unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Indexes {
    pub fields: Vec<FieldIndex>,
    pub text: Option<TextIndex>,
    pub ordinals: Ordinals,
}

fn invalid_index(field: &str) -> Error {
    Error::invalid_document("invalid_index", format!("invalid index on {field:?}"))
        .with_detail("field", field)
}

pub fn unique_violation(
    db: &str,
    coll: &str,
    field: &str,
    value: &Value,
    ids: Vec<String>,
) -> Error {
    Error::new(
        ErrorCode::UniqueViolation,
        format!("the value of {field} is already taken"),
    )
    .with_detail("db", db)
    .with_detail("collection", coll)
    .with_detail("field", field)
    .with_detail("value", value.clone())
    .with_detail("ids", ids)
}

fn array_refused(field: &str, ids: Vec<String>) -> Error {
    Error::invalid_document(
        "array_in_single_index",
        format!("{field} holds an array but its index is not multi"),
    )
    .with_detail("field", field)
    .with_detail("ids", ids)
}

fn doc_of(entry: &FileEntry) -> Option<&Doc> {
    match &entry.outcome {
        Outcome::Doc(found) => Some(&found.doc),
        _ => None,
    }
}

impl Indexes {
    pub fn dump(&self, lookup: &HashMap<&str, u32>) -> IndexDump {
        let ordinals = &self.ordinals;
        IndexDump {
            fields: self
                .fields
                .iter()
                .map(|index| FieldDump {
                    entries: index
                        .entries
                        .iter()
                        .map(|(key, set)| (key.0.clone(), positions(set, ordinals, lookup)))
                        .collect(),
                    unindexed: positions(&index.unindexed, ordinals, lookup),
                })
                .collect(),
            text: self.text.as_ref().map(|index| {
                index
                    .entries
                    .iter()
                    .map(|(token, set)| (token.clone(), positions(set, ordinals, lookup)))
                    .collect()
            }),
        }
    }

    pub fn restore(meta: &CollectionMeta, dump: IndexDump, ids: &[&str]) -> Option<Self> {
        let mut indexes = Self::empty(meta).ok()?;
        if indexes.fields.len() != dump.fields.len()
            || indexes.text.is_some() != dump.text.is_some()
        {
            return None;
        }
        for (index, saved) in indexes.fields.iter_mut().zip(dump.fields) {
            for (value, positions) in saved.entries {
                index
                    .entries
                    .insert_mut(Key(value), restored(&positions, ids.len())?);
            }
            index.unindexed = restored(&saved.unindexed, ids.len())?;
        }
        if let (Some(index), Some(saved)) = (&mut indexes.text, dump.text) {
            for (token, positions) in saved {
                index
                    .entries
                    .insert_mut(token, restored(&positions, ids.len())?);
            }
        }
        if indexes.tracks() {
            indexes.ordinals = Ordinals::listed(ids);
        }
        Some(indexes)
    }

    pub fn empty(meta: &CollectionMeta) -> Result<Self> {
        let mut fields = Vec::new();
        let mut text = None;
        for spec in &meta.indexes {
            match spec {
                IndexSpec::Field {
                    field,
                    unique,
                    multi,
                } => fields.push(FieldIndex::new(field, *unique, *multi)?),
                IndexSpec::Text { text: paths } => {
                    if text.is_some() || paths.is_empty() {
                        return Err(invalid_index("text"));
                    }
                    let named = paths
                        .iter()
                        .map(|path| split_path(path).map_err(|_| invalid_index(path)))
                        .collect::<Result<Vec<_>>>()?;
                    text = Some(TextFields::Named(named));
                }
            }
        }
        if text.is_none() && meta.format == Format::Md {
            text = Some(TextFields::All);
        }
        let text = text.map(|fields| TextIndex {
            fields,
            entries: RedBlackTreeMapSync::new_sync(),
        });
        Ok(Self {
            fields,
            text,
            ordinals: Ordinals::default(),
        })
    }

    fn tracks(&self) -> bool {
        !self.fields.is_empty() || self.text.is_some()
    }

    pub fn build<'a>(
        meta: &CollectionMeta,
        files: impl Iterator<Item = &'a FileEntry>,
    ) -> Result<Self> {
        let mut indexes = Self::empty(meta)?;
        if !indexes.tracks() {
            return Ok(indexes);
        }
        let docs: Vec<(u32, &Doc)> = files
            .filter_map(|entry| {
                let doc = doc_of(entry)?;
                Some((indexes.ordinals.assign(&entry.id), doc))
            })
            .collect();
        for index in &mut indexes.fields {
            index.bulk(&docs);
        }
        if let Some(index) = &mut indexes.text {
            index.bulk(&docs);
        }
        Ok(indexes)
    }

    pub fn apply(&mut self, old: Option<&FileEntry>, new: Option<&FileEntry>) {
        if !self.tracks() {
            return;
        }
        let old_doc = old.and_then(|entry| doc_of(entry).map(|doc| (entry.id.as_str(), doc)));
        let new_doc = new.and_then(|entry| doc_of(entry).map(|doc| (entry.id.as_str(), doc)));
        let old_ordinal = old_doc.and_then(|(id, doc)| Some((self.ordinals.of(id)?, doc)));
        if let Some((ordinal, doc)) = old_ordinal {
            for index in &mut self.fields {
                index.remove(ordinal, doc);
            }
        }
        let same =
            matches!((old_doc, new_doc), (Some((left, _)), Some((right, _))) if left == right);
        if !same && let Some(entry) = old {
            self.ordinals.release(&entry.id);
        }
        let new_ordinal = new_doc.map(|(id, doc)| (self.ordinals.assign(id), doc));
        if let Some((ordinal, doc)) = new_ordinal {
            for index in &mut self.fields {
                index.add(ordinal, doc);
            }
        }
        if let Some(index) = &mut self.text {
            match (old_ordinal, new_ordinal) {
                (Some((before, old)), Some((after, new))) if before == after => {
                    index.replace(after, Some(old), Some(new));
                }
                _ => {
                    if let Some((ordinal, doc)) = old_ordinal {
                        index.replace(ordinal, Some(doc), None);
                    }
                    if let Some((ordinal, doc)) = new_ordinal {
                        index.replace(ordinal, None, Some(doc));
                    }
                }
            }
        }
    }

    pub fn for_path(&self, path: &[String]) -> Option<&FieldIndex> {
        self.fields.iter().find(|index| index.path == path)
    }

    pub(crate) fn all_duplicates(&self) -> Vec<(&str, Value, Vec<String>)> {
        let mut found = Vec::new();
        for index in self.fields.iter().filter(|index| index.unique) {
            for (value, set) in index.duplicated() {
                let mut ids = self.ids(set);
                ids.sort();
                found.push((index.field.as_str(), value.clone(), ids));
            }
        }
        found
    }

    pub fn ids(&self, set: &RoaringBitmap) -> Vec<String> {
        self.ordinals.ids(set)
    }

    pub fn validate_new<'a>(
        db: &str,
        coll: &str,
        meta: &CollectionMeta,
        files: impl Iterator<Item = &'a FileEntry>,
    ) -> Result<()> {
        let built = Self::build(meta, files)?;
        for index in &built.fields {
            if !index.unindexed.is_empty() {
                return Err(array_refused(&index.field, built.ids(&index.unindexed)));
            }
            if index.unique
                && let Some((value, set)) = index.duplicates()
            {
                return Err(unique_violation(
                    db,
                    coll,
                    &index.field,
                    value,
                    built.ids(set),
                ));
            }
        }
        Ok(())
    }

    pub fn check_writes(&self, db: &str, coll: &str, writes: &[PendingWrite<'_>]) -> Result<()> {
        let touched: BTreeSet<&str> = writes.iter().map(|(id, _, _)| *id).collect();
        for index in &self.fields {
            let mut incoming: BTreeMap<Key, Vec<(String, usize)>> = BTreeMap::new();
            for (id, doc, op_index) in writes {
                let Some(doc) = doc else { continue };
                match index.keys_for(doc) {
                    Keys::Refused => {
                        return Err(array_refused(&index.field, vec![(*id).to_owned()])
                            .with_detail("op_index", *op_index));
                    }
                    Keys::Some(keys) if index.unique => {
                        for key in keys {
                            let holders = incoming.entry(Key(key)).or_default();
                            if !holders.iter().any(|(known, _)| known == id) {
                                holders.push(((*id).to_owned(), *op_index));
                            }
                        }
                    }
                    Keys::Some(_) => {}
                }
            }
            for (key, holders) in incoming {
                let existing: Vec<String> = index
                    .entries
                    .get(&key)
                    .map(|existing| {
                        self.ids(existing)
                            .into_iter()
                            .filter(|id| !touched.contains(id.as_str()))
                            .collect()
                    })
                    .unwrap_or_default();
                let second = usize::from(existing.is_empty());
                let Some((_, violating)) = holders.get(second) else {
                    continue;
                };
                let violating = *violating;
                let ids: Vec<String> = holders
                    .into_iter()
                    .map(|(id, _)| id)
                    .chain(existing)
                    .collect();
                return Err(unique_violation(db, coll, &index.field, &key.0, ids)
                    .with_detail("op_index", violating));
            }
        }
        Ok(())
    }
}

impl CollectionSnapshot {
    pub(crate) fn put_file(&mut self, name: &str, entry: Option<FileEntry>) {
        let old = self.files.get(name).cloned();
        self.indexes.apply(old.as_ref(), entry.as_ref());
        match entry {
            Some(entry) => self.files.insert_mut(name.to_owned(), entry),
            None => {
                self.files.remove_mut(name);
            }
        }
    }
}
