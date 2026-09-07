//! The [`Storelike`] the sync engine writes into.
//!
//! `syncables-rs` speaks plain JSON records and a neutral ontology
//! description (see [`syncables`]); this module is the half that knows
//! about Atomic Data. It renders both into Atomic Data `Resource`s and puts
//! them in a [`Storelike`], which is where the "reflection" actually lands.
//!
//! Two subject shapes are minted, both under the store's `internal:` space so
//! the whole set moves origins by changing `PUBLIC_URL` alone:
//!
//! * **Ontology terms** — `internal:/github-issues/property/title`, served as
//!   `https://my-ontologies.com/github-issues/property/title`. Classes and
//!   properties have to carry canonical, resolvable URLs, which is the reason
//!   the public URL is configuration rather than something guessed at runtime.
//! * **Records** — `internal:/<namespace>/<resource>/<id>`, e.g.
//!   `internal:/localthought%2Ftest-repo-1/issue/1`. Each segment is escaped,
//!   so a namespace containing `/` stays one segment and round-trips exactly.
//!
//! Records are typed with the ontology's own terms: a field named `title`
//! becomes the property the ontology declared with shortname `title`. The
//! engine's contract calls [`Storage::put_ontology`] before any record for
//! that reason — nothing in the store should refer to a term that isn't there.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use atomic_lib::datatype::match_datatype;
use atomic_lib::values::SubResource;
use atomic_lib::{urls, Resource, Storelike, Subject, Value};

use crate::ontology::{decode_segment, encode_segment, SubjectMapper};
use syncables::{
    ontology_shortname, Ontology, OntologyTerm, Record, Storage, StorageError, TermKind,
};

/// Sets a property on a Resource, surfacing the failure rather than dropping
/// it: `set_unsafe` writes through to the resource's CRDT document, so it can
/// genuinely fail and a silent `let _ =` would leave a half-written resource.
macro_rules! set {
    ($resource:expr, $property:expr, $value:expr $(,)?) => {
        $resource
            .set_unsafe($property, $value)
            .map_err(|error| StorageError::new(error.to_string()))?
    };
}

/// What [`AtomicStorage`] remembers from the stored ontology so records can be
/// typed with it.
#[derive(Clone, Debug, Default)]
struct TermIndex {
    /// Path of the ontology resource itself, used to mint a fallback term for
    /// a field the ontology did not declare.
    ontology_path: String,
    /// Property shortname → (internal subject, datatype URL, class path).
    properties: HashMap<String, (String, Option<String>, Option<String>)>,
    /// Class shortname → internal subject.
    classes: HashMap<String, String>,
}

/// An Atomic Data [`Storelike`] presented to the sync engine as a
/// [`Storage`].
///
/// The binary uses a persistent redb `Db`; unit tests can use an in-memory store.
pub struct AtomicStorage<S: Storelike> {
    store: Arc<S>,
    mapper: SubjectMapper,
    /// Filled by `put_ontology`; read on every record write. A lock rather
    /// than an `&mut self` because [`Storage`] hands out shared references.
    index: RwLock<TermIndex>,
    drive_owner: Option<String>,
}

impl<S: Storelike> AtomicStorage<S> {
    pub fn new(store: Arc<S>, mapper: SubjectMapper) -> Self {
        AtomicStorage {
            store,
            mapper,
            index: RwLock::new(TermIndex::default()),
            drive_owner: None,
        }
    }

    /// Grant this agent access when creating a drive; existing ACLs are retained.
    pub fn with_drive_owner(mut self, owner: Option<String>) -> Self {
        self.drive_owner = owner;
        self
    }

    pub fn drive_subject(&self, namespace: &str) -> String {
        self.mapper
            .internal(&format!("reflector-drives/{}", encode_segment(namespace)))
    }

    async fn ensure_drive(&self, namespace: &str) -> Result<Subject, StorageError> {
        let subject = Subject::from(self.drive_subject(namespace).as_str());
        if !self.store.has_stored_resource(&subject) {
            let mut drive = Resource::new(subject.to_string());
            set!(
                drive,
                urls::IS_A.to_owned(),
                Value::ResourceArray(vec![SubResource::Subject(Subject::from(urls::DRIVE))])
            );
            let index = self.index();
            let label = if index.ontology_path.is_empty() {
                "reflected data".to_owned()
            } else {
                index.ontology_path.replace('-', " ")
            };
            set!(
                drive,
                urls::NAME.to_owned(),
                Value::String(format!("{} {}", label, namespace.replace('/', " ")))
            );
            if let Some(owner) = &self.drive_owner {
                for permission in [urls::READ, urls::WRITE] {
                    set!(
                        drive,
                        permission.to_owned(),
                        Value::ResourceArray(vec![SubResource::Subject(Subject::from(
                            owner.as_str()
                        ))])
                    );
                }
            }
            self.store
                .add_resource(&drive)
                .await
                .map_err(|error| StorageError::new(error.to_string()))?;
        }
        Ok(subject)
    }

    /// Replace the projection while retaining the stored CRDT's history.
    async fn persist(&self, desired: &Resource, validate: bool) -> Result<(), StorageError> {
        let mut resource = if self.store.has_stored_resource(desired.get_subject()) {
            self.store
                .get_resource(desired.get_subject())
                .await
                .map_err(|error| StorageError::new(error.to_string()))?
        } else {
            Resource::new(desired.get_subject().to_string())
        };
        let removed: Vec<String> = resource
            .get_propvals()
            .keys()
            .filter(|property| {
                property.as_str() != urls::LORO_UPDATE
                    && !desired.get_propvals().contains_key(*property)
            })
            .cloned()
            .collect();
        for property in removed {
            resource
                .remove_propval(&property)
                .map_err(|error| StorageError::new(error.to_string()))?;
        }
        for (property, value) in desired.get_propvals() {
            set!(resource, property.clone(), value.clone());
        }
        self.store
            .add_resource_opts(&resource, validate, true, true)
            .await
            .map_err(|error| StorageError::new(error.to_string()))
    }

    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    pub fn mapper(&self) -> &SubjectMapper {
        &self.mapper
    }

    /// `internal:/<namespace>/<resource>/<id>`.
    pub fn record_subject(&self, namespace: &str, resource: &str, id: &str) -> String {
        self.mapper.internal(&format!(
            "{}/{}/{}",
            encode_segment(namespace),
            encode_segment(resource),
            encode_segment(id)
        ))
    }

    /// The `internal:/<namespace>/<resource>/` prefix every record of one
    /// resource shares — how `list` finds them without an index.
    fn record_prefix(&self, namespace: &str, resource: &str) -> String {
        self.mapper.internal(&format!(
            "{}/{}/",
            encode_segment(namespace),
            encode_segment(resource)
        ))
    }

    fn index(&self) -> TermIndex {
        self.index
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The property subject a record field is stored under. A field the
    /// ontology declared uses that term; anything else gets a deterministic
    /// subject under the ontology's own path, so an incomplete ontology
    /// costs correct typing but never loses data.
    fn property_for(
        &self,
        index: &TermIndex,
        field: &str,
    ) -> (String, Option<String>, Option<String>) {
        let shortname = ontology_shortname(field);
        if let Some(term) = index.properties.get(&shortname) {
            return term.clone();
        }
        let path = if index.ontology_path.is_empty() {
            format!("property/{}", encode_segment(&shortname))
        } else {
            format!(
                "{}/property/{}",
                index.ontology_path,
                encode_segment(&shortname)
            )
        };
        (self.mapper.internal(&path), None, None)
    }

    /// Turns one stored resource back into the record the engine put there.
    fn record_from_resource(
        &self,
        resource: &Resource,
        index: &TermIndex,
        resources: &HashMap<String, Resource>,
    ) -> Option<Record> {
        let subject = resource.get_subject().to_string();
        let path = subject.strip_prefix(crate::ontology::INTERNAL_PREFIX)?;
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() != 3 {
            return None;
        }
        // Reverse the shortname → subject map once per resource; the ontology
        // is small (tens of terms) and this keeps the index single-purpose.
        let mut shortnames: HashMap<&str, &str> = HashMap::new();
        for (shortname, (property_subject, _, _)) in &index.properties {
            shortnames.insert(property_subject.as_str(), shortname.as_str());
        }

        let mut value = serde_json::Map::new();
        for (property, stored) in resource.get_propvals() {
            if matches!(
                property.as_str(),
                urls::IS_A | urls::PARENT | urls::DRIVE_PROP | urls::LORO_UPDATE
            ) {
                continue;
            }
            let field = match shortnames.get(property.as_str()) {
                Some(shortname) => (*shortname).to_owned(),
                // Fallback terms are minted as `<ontology>/property/<field>`,
                // so the last segment is the field name.
                None => decode_segment(property.rsplit('/').next().unwrap_or(property)),
            };
            let follows_nested = index
                .properties
                .values()
                .any(|(subject, _, class_type)| subject == property && class_type.is_some());
            value.insert(
                field,
                self.json_from_value(stored, index, resources, follows_nested),
            );
        }

        Some(Record {
            namespace: decode_segment(segments[0]),
            resource: decode_segment(segments[1]),
            id: decode_segment(segments[2]),
            value,
        })
    }

    fn json_from_value(
        &self,
        value: &Value,
        index: &TermIndex,
        resources: &HashMap<String, Resource>,
        follows_nested: bool,
    ) -> serde_json::Value {
        match value {
            Value::AtomicUrl(subject)
                if follows_nested && resources.contains_key(&subject.to_string()) =>
            {
                self.object_from_resource(&resources[&subject.to_string()], index, resources)
            }
            Value::ResourceArray(items) if follows_nested => serde_json::Value::Array(
                items
                    .iter()
                    .map(|item| match item {
                        SubResource::Subject(subject)
                            if resources.contains_key(&subject.to_string()) =>
                        {
                            self.object_from_resource(
                                &resources[&subject.to_string()],
                                index,
                                resources,
                            )
                        }
                        SubResource::Subject(subject) => {
                            serde_json::Value::String(subject.to_string())
                        }
                        SubResource::Nested(_) => serde_json::Value::Null,
                    })
                    .collect(),
            ),
            other => json_from_value(other),
        }
    }

    fn object_from_resource(
        &self,
        resource: &Resource,
        index: &TermIndex,
        resources: &HashMap<String, Resource>,
    ) -> serde_json::Value {
        let reverse: HashMap<&str, &str> = index
            .properties
            .iter()
            .map(|(shortname, (subject, _, _))| (subject.as_str(), shortname.as_str()))
            .collect();
        let mut object = serde_json::Map::new();
        for (property, value) in resource.get_propvals() {
            if matches!(
                property.as_str(),
                urls::IS_A | urls::PARENT | urls::DRIVE_PROP | urls::LORO_UPDATE
            ) {
                continue;
            }
            let field = reverse
                .get(property.as_str())
                .map(|name| (*name).to_owned())
                .unwrap_or_else(|| decode_segment(property.rsplit('/').next().unwrap_or(property)));
            let follows_nested = index
                .properties
                .values()
                .any(|(subject, _, class_type)| subject == property && class_type.is_some());
            object.insert(
                field,
                self.json_from_value(value, index, resources, follows_nested),
            );
        }
        serde_json::Value::Object(object)
    }

    fn resources_for_object(
        &self,
        subject: String,
        class: Option<&str>,
        object: &serde_json::Map<String, serde_json::Value>,
        index: &TermIndex,
    ) -> Result<Vec<Resource>, StorageError> {
        let mut resource = Resource::new(subject.clone());
        if let Some(class) = class {
            set!(
                resource,
                urls::IS_A.to_owned(),
                Value::ResourceArray(vec![SubResource::Subject(Subject::from(
                    self.mapper.resolve_reference(class).as_str()
                ))])
            );
        }
        let mut resources = Vec::new();
        for (field, json) in object {
            if json.is_null() {
                continue;
            }
            let (property, datatype, class_type) = self.property_for(index, field);
            let value = match json {
                serde_json::Value::Object(child) => {
                    let child_subject = format!("{subject}/{}", encode_segment(field));
                    resources.extend(self.resources_for_object(
                        child_subject.clone(),
                        class_type.as_deref(),
                        child,
                        index,
                    )?);
                    Value::AtomicUrl(Subject::from(child_subject.as_str()))
                }
                serde_json::Value::Array(items)
                    if items.iter().all(serde_json::Value::is_object) =>
                {
                    let mut references = Vec::new();
                    for (position, item) in items.iter().enumerate() {
                        let child_subject =
                            format!("{subject}/{}/{position}", encode_segment(field));
                        resources.extend(self.resources_for_object(
                            child_subject.clone(),
                            class_type.as_deref(),
                            item.as_object().expect("checked object"),
                            index,
                        )?);
                        references
                            .push(SubResource::Subject(Subject::from(child_subject.as_str())));
                    }
                    Value::ResourceArray(references)
                }
                _ => value_from_json(json, datatype.as_deref())?,
            };
            set!(resource, property, value);
        }
        resources.push(resource);
        Ok(resources)
    }

    /// Renders one ontology term as an Atomic Data Class or Property.
    fn term_resource(
        &self,
        ontology: &Ontology,
        term: &OntologyTerm,
    ) -> Result<Resource, StorageError> {
        let mut resource = Resource::new(self.mapper.internal(&term.path));
        let class = match term.kind {
            TermKind::Class => urls::CLASS,
            TermKind::Property => urls::PROPERTY,
        };
        set!(
            resource,
            urls::IS_A.to_owned(),
            Value::ResourceArray(vec![SubResource::Subject(Subject::from(class))]),
        );
        set!(
            resource,
            urls::SHORTNAME.to_owned(),
            Value::Slug(term.shortname.clone()),
        );
        set!(
            resource,
            urls::DESCRIPTION.to_owned(),
            Value::Markdown(term.description.clone()),
        );
        set!(
            resource,
            urls::PARENT.to_owned(),
            Value::AtomicUrl(Subject::from(self.mapper.internal(&ontology.path).as_str())),
        );
        if let Some(datatype) = &term.datatype {
            set!(
                resource,
                urls::DATATYPE_PROP.to_owned(),
                Value::AtomicUrl(Subject::from(datatype.as_str())),
            );
        }
        if let Some(class_type) = &term.class_type {
            set!(
                resource,
                urls::CLASSTYPE_PROP.to_owned(),
                Value::AtomicUrl(Subject::from(
                    self.mapper.resolve_reference(class_type).as_str()
                )),
            );
        }
        if !term.requires.is_empty() {
            set!(
                resource,
                urls::REQUIRES.to_owned(),
                self.references(&term.requires)
            );
        }
        if !term.recommends.is_empty() {
            set!(
                resource,
                urls::RECOMMENDS.to_owned(),
                self.references(&term.recommends)
            );
        }
        Ok(resource)
    }

    fn references(&self, references: &[String]) -> Value {
        Value::ResourceArray(
            references
                .iter()
                .map(|reference| {
                    SubResource::Subject(Subject::from(
                        self.mapper.resolve_reference(reference).as_str(),
                    ))
                })
                .collect(),
        )
    }
}

// `Storelike`'s own methods are `?Send` on wasm32 (that target is
// single-threaded), so this impl's futures aren't `Send` there either;
// `syncables::Storage` relaxes its own bound the same way for exactly this
// case (see its doc comment).
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl<S: Storelike> Storage for AtomicStorage<S> {
    async fn put(&self, record: &Record) -> Result<(), StorageError> {
        let index = self.index();
        let subject = self.record_subject(&record.namespace, &record.resource, &record.id);
        let drive = self.ensure_drive(&record.namespace).await?;
        let stale_prefix = format!("{subject}/");
        let stale_subjects: Vec<Subject> = self
            .store
            .all_resources(false)
            .filter_map(|resource| {
                let candidate = resource.get_subject().to_string();
                candidate
                    .starts_with(&stale_prefix)
                    .then(|| Subject::from(candidate.as_str()))
            })
            .collect();
        let resources = self.resources_for_object(
            subject,
            index
                .classes
                .get(&ontology_shortname(&record.resource))
                .map(String::as_str),
            &record.value,
            &index,
        )?;
        let current_subjects: Vec<Subject> = resources
            .iter()
            .map(|resource| resource.get_subject().clone())
            .collect();
        for mut resource in resources {
            set!(
                resource,
                urls::PARENT.to_owned(),
                Value::AtomicUrl(drive.clone())
            );
            set!(
                resource,
                urls::DRIVE_PROP.to_owned(),
                Value::AtomicUrl(drive.clone())
            );
            self.persist(&resource, true).await?;
        }
        for stale in stale_subjects {
            if !current_subjects.contains(&stale) {
                self.store
                    .remove_resource(&stale)
                    .await
                    .map_err(|error| StorageError::new(error.to_string()))?;
            }
        }
        Ok(())
    }

    async fn get(
        &self,
        namespace: &str,
        resource: &str,
        id: &str,
    ) -> Result<Option<Record>, StorageError> {
        let subject = Subject::from(self.record_subject(namespace, resource, id).as_str());
        // `get_resource` would fall back to fetching the subject over the
        // network; a local-first copy must answer from the store alone.
        if !self.store.has_stored_resource(&subject) {
            return Ok(None);
        }
        let stored = self
            .store
            .get_resource(&subject)
            .await
            .map_err(|error| StorageError::new(error.to_string()))?;
        let resources: HashMap<String, Resource> = self
            .store
            .all_resources(false)
            .map(|resource| (resource.get_subject().to_string(), resource))
            .collect();
        Ok(self.record_from_resource(&stored, &self.index(), &resources))
    }

    async fn list(&self, namespace: &str, resource: &str) -> Result<Vec<Record>, StorageError> {
        let prefix = self.record_prefix(namespace, resource);
        let index = self.index();
        let resources: HashMap<String, Resource> = self
            .store
            .all_resources(false)
            .map(|stored| (stored.get_subject().to_string(), stored))
            .collect();
        let mut records: Vec<Record> = resources
            .values()
            .filter(|stored| {
                let subject = stored.get_subject().to_string();
                subject.starts_with(&prefix) && subject[prefix.len()..].split('/').count() == 1
            })
            .filter_map(|stored| self.record_from_resource(stored, &index, &resources))
            .collect();
        // `all_resources` iterates a hash map, so sort for a stable listing.
        records.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(records)
    }

    async fn delete(&self, namespace: &str, resource: &str, id: &str) -> Result<(), StorageError> {
        let subject = Subject::from(self.record_subject(namespace, resource, id).as_str());
        if !self.store.has_stored_resource(&subject) {
            return Ok(());
        }
        let prefix = format!("{subject}/");
        let mut subjects = vec![subject];
        subjects.extend(self.store.all_resources(false).filter_map(|resource| {
            let candidate = resource.get_subject().to_string();
            candidate
                .starts_with(&prefix)
                .then(|| Subject::from(candidate.as_str()))
        }));
        for subject in subjects {
            self.store
                .remove_resource(&subject)
                .await
                .map_err(|error| StorageError::new(error.to_string()))?;
        }
        Ok(())
    }

    async fn put_ontology(&self, ontology: &Ontology) -> Result<(), StorageError> {
        let mut term_index = TermIndex {
            ontology_path: ontology.path.trim_matches('/').to_owned(),
            ..TermIndex::default()
        };
        let mut classes: Vec<SubResource> = Vec::new();
        let mut properties: Vec<SubResource> = Vec::new();

        for term in &ontology.terms {
            let subject = self.mapper.internal(&term.path);
            match term.kind {
                TermKind::Class => {
                    term_index
                        .classes
                        .insert(term.shortname.clone(), subject.clone());
                    classes.push(SubResource::Subject(Subject::from(subject.as_str())));
                }
                TermKind::Property => {
                    term_index.properties.insert(
                        term.shortname.clone(),
                        (
                            subject.clone(),
                            term.datatype.clone(),
                            term.class_type.clone(),
                        ),
                    );
                    properties.push(SubResource::Subject(Subject::from(subject.as_str())));
                }
            }
            let resource = self.term_resource(ontology, term)?;
            self.persist(&resource, false).await?;
        }

        let mut ontology_resource = Resource::new(self.mapper.internal(&ontology.path));
        set!(
            ontology_resource,
            urls::IS_A.to_owned(),
            Value::ResourceArray(vec![SubResource::Subject(Subject::from(urls::ONTOLOGY))]),
        );
        set!(
            ontology_resource,
            urls::SHORTNAME.to_owned(),
            Value::Slug(ontology.shortname.clone()),
        );
        set!(
            ontology_resource,
            urls::DESCRIPTION.to_owned(),
            Value::Markdown(ontology.description.clone()),
        );
        set!(
            ontology_resource,
            urls::CLASSES.to_owned(),
            Value::ResourceArray(classes)
        );
        set!(
            ontology_resource,
            urls::PROPERTIES.to_owned(),
            Value::ResourceArray(properties)
        );
        self.persist(&ontology_resource, false).await?;

        *self
            .index
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = term_index;
        Ok(())
    }
}

/// JSON → Atomic Data. The ontology's datatype decides when it can (a string
/// the document typed as a timestamp is stored as one); otherwise the JSON
/// type does.
fn value_from_json(
    json: &serde_json::Value,
    datatype: Option<&str>,
) -> Result<Value, StorageError> {
    if let Some(datatype) = datatype {
        if datatype == urls::TIMESTAMP {
            let text = json
                .as_str()
                .ok_or_else(|| StorageError::new("a timestamp must be a JSON string"))?;
            let timestamp = chrono::DateTime::parse_from_rfc3339(text)
                .map_err(|error| StorageError::new(format!("invalid RFC 3339 timestamp: {error}")))?
                .timestamp_millis();
            return Ok(Value::Timestamp(timestamp));
        }
        let text = json
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| json.to_string());
        return Value::new(&text, &match_datatype(datatype))
            .map_err(|error| StorageError::new(error.to_string()));
    }
    Ok(match json {
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(integer) => Value::Integer(integer),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        serde_json::Value::String(text) => Value::String(text.clone()),
        // Object-valued properties and arrays of objects are handled by
        // `resources_for_object`; remaining arrays keep their primitive JSON.
        other => Value::Json(other.clone()),
    })
}

/// Atomic Data → JSON, the inverse of [`value_from_json`] for the variants
/// this crate stores.
fn json_from_value(value: &Value) -> serde_json::Value {
    match value {
        Value::Boolean(value) => serde_json::Value::Bool(*value),
        Value::Integer(value) | Value::Timestamp(value) => serde_json::Value::from(*value),
        Value::Float(value) => serde_json::Value::from(*value),
        Value::Json(value) => value.clone(),
        Value::ResourceArray(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| match item {
                    SubResource::Subject(subject) => serde_json::Value::String(subject.to_string()),
                    SubResource::Nested(_) => serde_json::Value::Null,
                })
                .collect(),
        ),
        other => serde_json::Value::String(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomic_lib::Store;

    fn ontology() -> Ontology {
        Ontology {
            path: "github-issues".to_owned(),
            shortname: "github-issues".to_owned(),
            description: "Derived from the GitHub Issues OpenAPI document.".to_owned(),
            terms: vec![
                OntologyTerm {
                    path: "github-issues/class/issue".to_owned(),
                    kind: TermKind::Class,
                    shortname: "issue".to_owned(),
                    description: "An issue in a repository.".to_owned(),
                    datatype: None,
                    class_type: None,
                    requires: vec!["github-issues/property/title".to_owned()],
                    recommends: vec!["github-issues/property/number".to_owned()],
                },
                OntologyTerm {
                    path: "github-issues/property/title".to_owned(),
                    kind: TermKind::Property,
                    shortname: "title".to_owned(),
                    description: "The issue's title.".to_owned(),
                    datatype: Some(urls::STRING.to_owned()),
                    class_type: None,
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/number".to_owned(),
                    kind: TermKind::Property,
                    shortname: "number".to_owned(),
                    description: "Per-repository issue number.".to_owned(),
                    datatype: Some(urls::INTEGER.to_owned()),
                    class_type: None,
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/body".to_owned(),
                    kind: TermKind::Property,
                    shortname: "body".to_owned(),
                    description: "The comment body.".to_owned(),
                    datatype: Some(urls::STRING.to_owned()),
                    class_type: None,
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/updated-at".to_owned(),
                    kind: TermKind::Property,
                    shortname: "updated-at".to_owned(),
                    description: "When the resource was updated.".to_owned(),
                    datatype: Some(urls::TIMESTAMP.to_owned()),
                    class_type: None,
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/class/issuecomment".to_owned(),
                    kind: TermKind::Class,
                    shortname: "issuecomment".to_owned(),
                    description: "A comment on an issue.".to_owned(),
                    datatype: None,
                    class_type: None,
                    requires: vec!["github-issues/property/body".to_owned()],
                    recommends: vec!["github-issues/property/updated-at".to_owned()],
                },
                OntologyTerm {
                    path: "github-issues/class/user".to_owned(),
                    kind: TermKind::Class,
                    shortname: "user".to_owned(),
                    description: "A GitHub user.".to_owned(),
                    datatype: None,
                    class_type: None,
                    requires: vec![],
                    recommends: vec!["github-issues/property/login".to_owned()],
                },
                OntologyTerm {
                    path: "github-issues/class/labels".to_owned(),
                    kind: TermKind::Class,
                    shortname: "labels".to_owned(),
                    description: "A GitHub label.".to_owned(),
                    datatype: None,
                    class_type: None,
                    requires: vec![],
                    recommends: vec!["github-issues/property/name".to_owned()],
                },
                OntologyTerm {
                    path: "github-issues/property/user".to_owned(),
                    kind: TermKind::Property,
                    shortname: "user".to_owned(),
                    description: "The author.".to_owned(),
                    datatype: Some(urls::ATOMIC_URL.to_owned()),
                    class_type: Some("github-issues/class/user".to_owned()),
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/labels".to_owned(),
                    kind: TermKind::Property,
                    shortname: "labels".to_owned(),
                    description: "Issue labels.".to_owned(),
                    datatype: Some(urls::RESOURCE_ARRAY.to_owned()),
                    class_type: Some("github-issues/class/labels".to_owned()),
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/login".to_owned(),
                    kind: TermKind::Property,
                    shortname: "login".to_owned(),
                    description: "The username.".to_owned(),
                    datatype: Some(urls::STRING.to_owned()),
                    class_type: None,
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/name".to_owned(),
                    kind: TermKind::Property,
                    shortname: "name".to_owned(),
                    description: "The label name.".to_owned(),
                    datatype: Some(urls::STRING.to_owned()),
                    class_type: None,
                    requires: vec![],
                    recommends: vec![],
                },
            ],
        }
    }

    fn record() -> Record {
        let mut value = serde_json::Map::new();
        value.insert("title".to_owned(), serde_json::json!("First issue"));
        value.insert("number".to_owned(), serde_json::json!(1));
        value.insert("body".to_owned(), serde_json::Value::Null);
        Record {
            namespace: "localthought/test-repo-1".to_owned(),
            resource: "issue".to_owned(),
            id: "1".to_owned(),
            value,
        }
    }

    async fn storage() -> AtomicStorage<Store> {
        let store = Store::init().await.expect("store");
        store.set_base_url("https://my-ontologies.com");
        AtomicStorage::new(
            Arc::new(store),
            SubjectMapper::new("https://my-ontologies.com"),
        )
    }

    #[tokio::test]
    async fn redb_survives_restart_and_preserves_other_drives_and_crdt_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut issue = record();
        issue
            .value
            .insert("user".into(), serde_json::json!({"login": "alice"}));
        let mut other = issue.clone();
        other.namespace = "another/repository".into();
        let owner = "did:ad:agent:test-owner";
        let original_version;
        {
            let db = atomic_lib::Db::init_redb_file(
                dir.path(),
                Some("https://my-ontologies.com".into()),
                dir.path(),
            )
            .await
            .unwrap();
            let mut main_drive = Resource::new("internal:/user-main-drive".into());
            main_drive
                .set_unsafe(urls::NAME.into(), Value::String("My main drive".into()))
                .unwrap();
            db.add_resource(&main_drive).await.unwrap();
            let storage = AtomicStorage::new(
                Arc::new(db),
                SubjectMapper::new("https://my-ontologies.com"),
            )
            .with_drive_owner(Some(owner.into()));
            storage.put_ontology(&ontology()).await.unwrap();
            storage.put(&issue).await.unwrap();
            storage.put(&other).await.unwrap();
            let subject = Subject::from(
                storage
                    .record_subject(&issue.namespace, &issue.resource, &issue.id)
                    .as_str(),
            );
            original_version = storage
                .store
                .get_resource(&subject)
                .await
                .unwrap()
                .build_state_doc()
                .unwrap()
                .oplog_vv_map();
        }
        assert!(dir.path().join("atomic.redb").is_file());
        {
            let db = atomic_lib::Db::init_redb_file(
                dir.path(),
                Some("https://my-ontologies.com".into()),
                dir.path(),
            )
            .await
            .unwrap();
            let storage = AtomicStorage::new(
                Arc::new(db),
                SubjectMapper::new("https://my-ontologies.com"),
            );
            storage.put_ontology(&ontology()).await.unwrap();
            assert_eq!(
                storage
                    .get(&issue.namespace, &issue.resource, &issue.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .value["user"],
                issue.value["user"]
            );
            let drive = Subject::from(storage.drive_subject(&issue.namespace).as_str());
            let stored_drive = storage.store.get_resource(&drive).await.unwrap();
            assert!(stored_drive.get_propvals()[urls::WRITE]
                .to_string()
                .contains(owner));
            let child = Subject::from(
                format!(
                    "{}/user",
                    storage.record_subject(&issue.namespace, &issue.resource, &issue.id)
                )
                .as_str(),
            );
            assert_eq!(
                storage
                    .store
                    .get_resource(&child)
                    .await
                    .unwrap()
                    .get_propvals()[urls::DRIVE_PROP]
                    .to_string(),
                drive.to_string()
            );
            issue
                .value
                .insert("title".into(), serde_json::json!("Updated"));
            issue.value.remove("user");
            storage.put(&issue).await.unwrap();
            assert!(!storage.store.has_stored_resource(&child));
            let subject = Subject::from(
                storage
                    .record_subject(&issue.namespace, &issue.resource, &issue.id)
                    .as_str(),
            );
            let updated = storage.store.get_resource(&subject).await.unwrap();
            let version = updated.build_state_doc().unwrap().oplog_vv_map();
            for (peer, counter) in original_version {
                assert!(
                    version.get(&peer).is_some_and(|new| *new >= counter),
                    "lost CRDT history"
                );
            }
            assert_eq!(
                storage
                    .store
                    .get_resource(&Subject::from("internal:/user-main-drive"))
                    .await
                    .unwrap()
                    .get_propvals()[urls::NAME]
                    .to_string(),
                "My main drive"
            );
            storage
                .delete(&issue.namespace, &issue.resource, &issue.id)
                .await
                .unwrap();
        }
        let db = atomic_lib::Db::init_redb_file(
            dir.path(),
            Some("https://my-ontologies.com".into()),
            dir.path(),
        )
        .await
        .unwrap();
        let storage = AtomicStorage::new(
            Arc::new(db),
            SubjectMapper::new("https://my-ontologies.com"),
        );
        storage.put_ontology(&ontology()).await.unwrap();
        assert!(storage
            .get(&issue.namespace, &issue.resource, &issue.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            storage
                .list(&other.namespace, &other.resource)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_ne!(
            storage.drive_subject(&issue.namespace),
            storage.drive_subject(&other.namespace)
        );
    }

    #[tokio::test]
    async fn records_belong_to_a_repository_drive() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.unwrap();
        let record = record();
        storage.put(&record).await.unwrap();
        let stored = storage
            .store
            .get_resource(&Subject::from(
                storage
                    .record_subject(&record.namespace, &record.resource, &record.id)
                    .as_str(),
            ))
            .await
            .unwrap();
        let parent = stored
            .get_propvals()
            .get(urls::PARENT)
            .expect("record needs a drive")
            .to_string();
        let drive = storage
            .store
            .get_resource(&Subject::from(parent.as_str()))
            .await
            .unwrap();
        assert_eq!(
            drive.get_propvals()[urls::NAME].to_string(),
            "github issues localthought test-repo-1"
        );
        assert_eq!(stored.get_propvals()[urls::DRIVE_PROP].to_string(), parent);
        let roundtrip = storage
            .get(&record.namespace, &record.resource, &record.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!roundtrip.value.contains_key("parent"));
        assert!(!roundtrip.value.contains_key("drive"));
    }

    #[tokio::test]
    async fn ontology_terms_are_stored_at_their_public_paths() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");

        let subject = Subject::from("internal:/github-issues/property/title");
        let stored = storage.store.get_resource(&subject).await.expect("term");
        assert_eq!(
            stored.get(urls::SHORTNAME).unwrap().to_string(),
            "title".to_owned()
        );
        // The stored subject is exactly the internal form of the public URL a
        // consumer would dereference.
        assert_eq!(
            storage.mapper.to_public(subject.as_str()).unwrap(),
            "https://my-ontologies.com/github-issues/property/title"
        );
    }

    #[tokio::test]
    async fn the_ontology_resource_lists_its_classes_and_properties() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");

        let stored = storage
            .store
            .get_resource(&Subject::from("internal:/github-issues"))
            .await
            .expect("ontology resource");
        let classes = stored.get(urls::CLASSES).unwrap().to_string();
        assert!(
            classes.contains("internal:/github-issues/class/issue"),
            "{classes}"
        );
        let properties = stored.get(urls::PROPERTIES).unwrap().to_string();
        assert!(
            properties.contains("internal:/github-issues/property/title"),
            "{properties}"
        );
    }

    #[tokio::test]
    async fn records_are_typed_with_the_ontology_and_round_trip() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        storage.put(&record()).await.expect("put");

        let stored = storage
            .store
            .get_resource(&Subject::from(
                "internal:/localthought%2Ftest-repo-1/issue/1",
            ))
            .await
            .expect("record");
        assert!(stored
            .get(urls::IS_A)
            .unwrap()
            .to_string()
            .contains("internal:/github-issues/class/issue"));
        assert_eq!(
            stored
                .get("internal:/github-issues/property/title")
                .unwrap()
                .to_string(),
            "First issue"
        );
        assert!(matches!(
            stored
                .get("internal:/github-issues/property/number")
                .unwrap(),
            Value::Integer(1)
        ));

        let read = storage
            .get("localthought/test-repo-1", "issue", "1")
            .await
            .expect("get")
            .expect("some");
        assert_eq!(read.namespace, "localthought/test-repo-1");
        assert_eq!(read.resource, "issue");
        assert_eq!(read.id, "1");
        assert_eq!(read.value["title"], serde_json::json!("First issue"));
        assert_eq!(read.value["number"], serde_json::json!(1));
        // A JSON null is an absent property, not a stored placeholder.
        assert!(!read.value.contains_key("body"));
    }

    #[tokio::test]
    async fn a_field_the_ontology_did_not_declare_is_still_stored() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        let mut record = record();
        record
            .value
            .insert("state".to_owned(), serde_json::json!("open"));
        storage.put(&record).await.expect("put");

        let read = storage
            .get("localthought/test-repo-1", "issue", "1")
            .await
            .expect("get")
            .expect("some");
        assert_eq!(read.value["state"], serde_json::json!("open"));
    }

    #[tokio::test]
    async fn nested_objects_and_array_items_are_separate_typed_resources() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        let mut record = record();
        record
            .value
            .insert("user".to_owned(), serde_json::json!({ "login": "octocat" }));
        record.value.insert(
            "labels".to_owned(),
            serde_json::json!([{ "name": "bug" }, { "name": "help wanted" }]),
        );
        storage.put(&record).await.expect("put");

        let root = "internal:/localthought%2Ftest-repo-1/issue/1";
        let stored = storage
            .store
            .get_resource(&Subject::from(format!("{root}/user").as_str()))
            .await
            .expect("user Thing");
        assert!(stored
            .get(urls::IS_A)
            .unwrap()
            .to_string()
            .contains("internal:/github-issues/class/user"));
        assert_eq!(
            stored
                .get("internal:/github-issues/property/login")
                .unwrap()
                .to_string(),
            "octocat"
        );
        assert!(storage
            .store
            .has_stored_resource(&Subject::from(format!("{root}/labels/0").as_str())));
        assert!(storage
            .store
            .has_stored_resource(&Subject::from(format!("{root}/labels/1").as_str())));

        let read = storage
            .get("localthought/test-repo-1", "issue", "1")
            .await
            .expect("get")
            .expect("record");
        assert_eq!(
            read.value["user"],
            serde_json::json!({ "login": "octocat" })
        );
        assert_eq!(
            read.value["labels"],
            serde_json::json!([{ "name": "bug" }, { "name": "help wanted" }])
        );

        storage
            .delete("localthought/test-repo-1", "issue", "1")
            .await
            .expect("delete");
        assert!(!storage
            .store
            .has_stored_resource(&Subject::from(format!("{root}/user").as_str())));
        assert!(!storage
            .store
            .has_stored_resource(&Subject::from(format!("{root}/labels/0").as_str())));
    }

    #[tokio::test]
    async fn raw_api_names_resolve_to_the_generated_ontology_terms() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        let record = Record {
            namespace: "localthought/test-repo-1/1".to_owned(),
            resource: "issueComment".to_owned(),
            id: "5449492104".to_owned(),
            value: serde_json::json!({
                "body": "A readable comment",
                "updated_at": "2026-08-28T07:02:45Z",
            })
            .as_object()
            .expect("object")
            .clone(),
        };
        storage.put(&record).await.expect("put comment");

        let stored = storage
            .store
            .get_resource(&Subject::from(
                "internal:/localthought%2Ftest-repo-1%2F1/issueComment/5449492104",
            ))
            .await
            .expect("comment");
        assert!(stored
            .get(urls::IS_A)
            .expect("class")
            .to_string()
            .contains("internal:/github-issues/class/issuecomment"));
        assert!(matches!(
            stored
                .get("internal:/github-issues/property/updated-at")
                .expect("normalized timestamp property"),
            Value::Timestamp(_)
        ));
        assert!(stored
            .get("internal:/github-issues/property/updated_at")
            .is_err());
    }

    #[tokio::test]
    async fn rejects_a_record_missing_a_required_ontology_property() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        let mut incomplete = record();
        incomplete.value.remove("title");

        let error = storage.put(&incomplete).await.expect_err("missing title");
        assert!(error.to_string().contains("property/title"), "{error}");
    }

    #[tokio::test]
    async fn rejects_a_malformed_timestamp_for_a_declared_property() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        let record = Record {
            namespace: "localthought/test-repo-1/1".to_owned(),
            resource: "issueComment".to_owned(),
            id: "5449492104".to_owned(),
            value: serde_json::json!({
                "body": "A readable comment",
                "updated_at": "not-a-timestamp",
            })
            .as_object()
            .expect("object")
            .clone(),
        };

        let error = storage.put(&record).await.expect_err("malformed timestamp");
        assert!(
            error.to_string().contains("invalid RFC 3339 timestamp"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn listing_and_deleting_are_scoped_to_one_namespace() {
        let storage = storage().await;
        storage.put_ontology(&ontology()).await.expect("ontology");
        storage.put(&record()).await.expect("put");
        let mut other = record();
        other.namespace = "localthought/other-repo".to_owned();
        storage.put(&other).await.expect("put other");

        let listed = storage
            .list("localthought/test-repo-1", "issue")
            .await
            .expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "1");

        storage
            .delete("localthought/test-repo-1", "issue", "1")
            .await
            .expect("delete");
        assert!(storage
            .list("localthought/test-repo-1", "issue")
            .await
            .expect("list")
            .is_empty());
        // The lookalike namespace is untouched.
        assert_eq!(
            storage
                .list("localthought/other-repo", "issue")
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn missing_records_are_none_rather_than_a_network_fetch() {
        let storage = storage().await;
        assert!(storage
            .get("localthought/test-repo-1", "issue", "404")
            .await
            .expect("get")
            .is_none());
    }
}
