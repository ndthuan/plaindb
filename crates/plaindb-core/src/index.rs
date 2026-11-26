use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;

use rpds::{RedBlackTreeMapSync, RedBlackTreeSetSync};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec::Doc;
use crate::config::{CollectionMeta, Format, IndexSpec};
use crate::error::{Error, ErrorCode, Result};
use crate::query::{Key, split_path};
use crate::query::{crosses_array, elements, terminals};
use crate::snapshot::{CollectionSnapshot, FileEntry, Outcome};
use crate::text::{TextFields, doc_tokens, tokens};

pub type PendingWrite<'a> = (&'a str, Option<&'a Doc>);
pub type IdSet = RedBlackTreeSetSync<String>;

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

fn ordinals(set: &IdSet, lookup: &HashMap<&str, u32>) -> Vec<u32> {
    set.iter()
        .filter_map(|id| lookup.get(id.as_str()).copied())
        .collect()
}

fn restored_set(ordinals: &[u32], ids: &[&str]) -> Option<IdSet> {
    let mut set = IdSet::new_sync();
    for ordinal in ordinals {
        let id = ids.get(usize::try_from(*ordinal).ok()?)?;
        set.insert_mut((*id).to_owned());
    }
    Some(set)
}

fn id_set(ids: &[&str]) -> IdSet {
    let mut set = IdSet::new_sync();
    for id in ids {
        set.insert_mut((*id).to_owned());
    }
    set
}

#[derive(Debug, Clone)]
pub struct FieldIndex {
    pub field: String,
    pub path: Vec<String>,
    pub unique: bool,
    pub multi: bool,
    entries: RedBlackTreeMapSync<Key, IdSet>,
    unindexed: IdSet,
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
            unindexed: IdSet::new_sync(),
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

    fn add(&mut self, id: &str, doc: &Doc) {
        match self.keys_for(doc) {
            Keys::Refused => self.unindexed.insert_mut(id.to_owned()),
            Keys::Some(keys) => {
                for key in keys {
                    let mut ids = self
                        .entries
                        .get(&Key(key.clone()))
                        .cloned()
                        .unwrap_or_else(IdSet::new_sync);
                    ids.insert_mut(id.to_owned());
                    self.entries.insert_mut(Key(key), ids);
                }
            }
        }
    }

    fn bulk(&mut self, docs: &[(&str, &Doc)]) {
        let mut grouped: BTreeMap<Key, Vec<&str>> = BTreeMap::new();
        for (id, doc) in docs {
            match self.keys_for(doc) {
                Keys::Refused => self.unindexed.insert_mut((*id).to_owned()),
                Keys::Some(keys) => {
                    for key in keys {
                        grouped.entry(Key(key)).or_default().push(id);
                    }
                }
            }
        }
        for (key, ids) in grouped {
            self.entries.insert_mut(key, id_set(&ids));
        }
    }

    fn remove(&mut self, id: &str, doc: &Doc) {
        match self.keys_for(doc) {
            Keys::Refused => {
                self.unindexed.remove_mut(id);
            }
            Keys::Some(keys) => {
                for key in keys {
                    let key = Key(key);
                    let Some(mut ids) = self.entries.get(&key).cloned() else {
                        continue;
                    };
                    ids.remove_mut(id);
                    if ids.is_empty() {
                        self.entries.remove_mut(&key);
                    } else {
                        self.entries.insert_mut(key, ids);
                    }
                }
            }
        }
    }

    pub fn exact(&self, value: &Value) -> Vec<String> {
        self.entries
            .get(&Key(value.clone()))
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn range(&self, from: Bound<Value>, to: Bound<Value>) -> Vec<String> {
        let wrap = |bound: Bound<Value>| match bound {
            Bound::Included(value) => Bound::Included(Key(value)),
            Bound::Excluded(value) => Bound::Excluded(Key(value)),
            Bound::Unbounded => Bound::Unbounded,
        };
        self.entries
            .range((wrap(from), wrap(to)))
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect()
    }

    pub fn prefix(&self, prefix: &str) -> Vec<String> {
        self.entries
            .range((
                Bound::Included(Key(Value::String(prefix.to_owned()))),
                Bound::Unbounded,
            ))
            .take_while(
                |(key, _)| matches!(&key.0, Value::String(text) if text.starts_with(prefix)),
            )
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect()
    }

    pub fn unindexed(&self) -> Vec<String> {
        self.unindexed.iter().cloned().collect()
    }

    fn duplicates(&self) -> Vec<(Value, Vec<String>)> {
        self.entries
            .iter()
            .filter(|(_, ids)| ids.size() > 1)
            .map(|(key, ids)| (key.0.clone(), ids.iter().cloned().collect()))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct TextIndex {
    pub fields: TextFields,
    entries: RedBlackTreeMapSync<String, IdSet>,
}

impl TextIndex {
    fn add(&mut self, id: &str, doc: &Doc) {
        for token in doc_tokens(doc, &self.fields) {
            let mut ids = self
                .entries
                .get(&token)
                .cloned()
                .unwrap_or_else(IdSet::new_sync);
            ids.insert_mut(id.to_owned());
            self.entries.insert_mut(token, ids);
        }
    }

    fn bulk(&mut self, docs: &[(&str, &Doc)]) {
        let mut grouped: HashMap<String, Vec<&str>> = HashMap::new();
        for (id, doc) in docs {
            for token in doc_tokens(doc, &self.fields) {
                grouped.entry(token).or_default().push(id);
            }
        }
        for (token, ids) in grouped {
            self.entries.insert_mut(token, id_set(&ids));
        }
    }

    fn remove(&mut self, id: &str, doc: &Doc) {
        for token in doc_tokens(doc, &self.fields) {
            let Some(mut ids) = self.entries.get(&token).cloned() else {
                continue;
            };
            ids.remove_mut(id);
            if ids.is_empty() {
                self.entries.remove_mut(&token);
            } else {
                self.entries.insert_mut(token, ids);
            }
        }
    }

    pub fn matching(&self, query: &str) -> Vec<String> {
        let mut found: Option<BTreeSet<String>> = None;
        for token in tokens(query) {
            let ids: BTreeSet<String> = self
                .entries
                .get(&token)
                .map(|ids| ids.iter().cloned().collect())
                .unwrap_or_default();
            found = Some(match found {
                Some(known) => known.intersection(&ids).cloned().collect(),
                None => ids,
            });
        }
        found.unwrap_or_default().into_iter().collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Indexes {
    pub fields: Vec<FieldIndex>,
    pub text: Option<TextIndex>,
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
        IndexDump {
            fields: self
                .fields
                .iter()
                .map(|index| FieldDump {
                    entries: index
                        .entries
                        .iter()
                        .map(|(key, ids)| (key.0.clone(), ordinals(ids, lookup)))
                        .collect(),
                    unindexed: ordinals(&index.unindexed, lookup),
                })
                .collect(),
            text: self.text.as_ref().map(|index| {
                index
                    .entries
                    .iter()
                    .map(|(token, ids)| (token.clone(), ordinals(ids, lookup)))
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
            for (value, ordinals) in saved.entries {
                index
                    .entries
                    .insert_mut(Key(value), restored_set(&ordinals, ids)?);
            }
            index.unindexed = restored_set(&saved.unindexed, ids)?;
        }
        if let (Some(index), Some(saved)) = (&mut indexes.text, dump.text) {
            for (token, ordinals) in saved {
                index
                    .entries
                    .insert_mut(token, restored_set(&ordinals, ids)?);
            }
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
        Ok(Self { fields, text })
    }

    pub fn build<'a>(
        meta: &CollectionMeta,
        files: impl Iterator<Item = &'a FileEntry>,
    ) -> Result<Self> {
        let mut indexes = Self::empty(meta)?;
        let docs: Vec<(&str, &Doc)> = files
            .filter_map(|entry| doc_of(entry).map(|doc| (entry.id.as_str(), doc)))
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
        for index in &mut self.fields {
            if let Some(old) = old
                && let Some(doc) = doc_of(old)
            {
                index.remove(&old.id, doc);
            }
            if let Some(new) = new
                && let Some(doc) = doc_of(new)
            {
                index.add(&new.id, doc);
            }
        }
        if let Some(index) = &mut self.text {
            if let Some(old) = old
                && let Some(doc) = doc_of(old)
            {
                index.remove(&old.id, doc);
            }
            if let Some(new) = new
                && let Some(doc) = doc_of(new)
            {
                index.add(&new.id, doc);
            }
        }
    }

    pub fn for_path(&self, path: &[String]) -> Option<&FieldIndex> {
        self.fields.iter().find(|index| index.path == path)
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
                return Err(array_refused(&index.field, index.unindexed()));
            }
            if index.unique
                && let Some((value, ids)) = index.duplicates().into_iter().next()
            {
                return Err(unique_violation(db, coll, &index.field, &value, ids));
            }
        }
        Ok(())
    }

    pub fn check_writes(&self, db: &str, coll: &str, writes: &[PendingWrite<'_>]) -> Result<()> {
        let touched: BTreeSet<&str> = writes.iter().map(|(id, _)| *id).collect();
        for index in &self.fields {
            let mut incoming: BTreeMap<Key, Vec<String>> = BTreeMap::new();
            for (id, doc) in writes {
                let Some(doc) = doc else { continue };
                match index.keys_for(doc) {
                    Keys::Refused => {
                        return Err(array_refused(&index.field, vec![(*id).to_owned()]));
                    }
                    Keys::Some(keys) if index.unique => {
                        for key in keys {
                            let ids = incoming.entry(Key(key)).or_default();
                            if !ids.iter().any(|known| known == id) {
                                ids.push((*id).to_owned());
                            }
                        }
                    }
                    Keys::Some(_) => {}
                }
            }
            for (key, mut ids) in incoming {
                if let Some(existing) = index.entries.get(&key) {
                    ids.extend(
                        existing
                            .iter()
                            .filter(|id| !touched.contains(id.as_str()))
                            .cloned(),
                    );
                }
                if ids.len() > 1 {
                    return Err(unique_violation(db, coll, &index.field, &key.0, ids));
                }
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
