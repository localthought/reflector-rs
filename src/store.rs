//! The [`Storelike`] the sync engine writes into.
//!
//! `syncables-rs` speaks plain JSON records and a neutral ontology
//! description (see [`crate::syncables`]); this module is the half that knows
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
use crate::syncables::{Ontology, OntologyTerm, Record, Storage, StorageError, TermKind};

/// Sets a property on a Resource, surfacing the failure rather than dropping
/// it: `set_unsafe` writes through to the resource's CRDT document, so it can
/// genuinely fail and a silent `let _ =` would leave a half-written resource.
macro_rules! set {
    ($resource:expr, $property:expr, $value:expr $(,)?) => {
        $resource
            .set_unsafe($property, $value)
            .map_err(|error| StorageError(error.to_string()))?
    };
}

/// What [`AtomicStorage`] remembers from the stored ontology so records can be
/// typed with it.
#[derive(Clone, Debug, Default)]
struct TermIndex {
    /// Path of the ontology resource itself, used to mint a fallback term for
    /// a field the ontology did not declare.
    ontology_path: String,
    /// Property shortname → (internal subject, datatype URL).
    properties: HashMap<String, (String, Option<String>)>,
    /// Class shortname → internal subject.
    classes: HashMap<String, String>,
}

/// An Atomic Data [`Storelike`] presented to the sync engine as a
/// [`Storage`].
///
/// Generic over the store: the in-memory `atomic_lib::Store` is enough for a
/// scaffold, and a persistent `Db` (or a remote store) drops in unchanged.
pub struct AtomicStorage<S: Storelike> {
    store: Arc<S>,
    mapper: SubjectMapper,
    /// Filled by `put_ontology`; read on every record write. A lock rather
    /// than an `&mut self` because [`Storage`] hands out shared references.
    index: RwLock<TermIndex>,
}

impl<S: Storelike> AtomicStorage<S> {
    pub fn new(store: Arc<S>, mapper: SubjectMapper) -> Self {
        AtomicStorage {
            store,
            mapper,
            index: RwLock::new(TermIndex::default()),
        }
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
    fn property_for(&self, index: &TermIndex, field: &str) -> (String, Option<String>) {
        if let Some(term) = index.properties.get(field) {
            return term.clone();
        }
        let path = if index.ontology_path.is_empty() {
            format!("property/{}", encode_segment(field))
        } else {
            format!("{}/property/{}", index.ontology_path, encode_segment(field))
        };
        (self.mapper.internal(&path), None)
    }

    /// Turns one stored resource back into the record the engine put there.
    fn record_from_resource(&self, resource: &Resource, index: &TermIndex) -> Option<Record> {
        let subject = resource.get_subject().to_string();
        let path = subject.strip_prefix(crate::ontology::INTERNAL_PREFIX)?;
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() != 3 {
            return None;
        }
        // Reverse the shortname → subject map once per resource; the ontology
        // is small (tens of terms) and this keeps the index single-purpose.
        let mut shortnames: HashMap<&str, &str> = HashMap::new();
        for (shortname, (property_subject, _)) in &index.properties {
            shortnames.insert(property_subject.as_str(), shortname.as_str());
        }

        let mut value = serde_json::Map::new();
        for (property, stored) in resource.get_propvals() {
            if property == urls::IS_A {
                continue;
            }
            let field = match shortnames.get(property.as_str()) {
                Some(shortname) => (*shortname).to_owned(),
                // Fallback terms are minted as `<ontology>/property/<field>`,
                // so the last segment is the field name.
                None => decode_segment(property.rsplit('/').next().unwrap_or(property)),
            };
            value.insert(field, json_from_value(stored));
        }

        Some(Record {
            namespace: decode_segment(segments[0]),
            resource: decode_segment(segments[1]),
            id: decode_segment(segments[2]),
            value,
        })
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

#[async_trait]
impl<S: Storelike> Storage for AtomicStorage<S> {
    async fn put(&self, record: &Record) -> Result<(), StorageError> {
        let index = self.index();
        let subject = self.record_subject(&record.namespace, &record.resource, &record.id);
        let mut resource = Resource::new(subject);

        if let Some(class) = index.classes.get(&record.resource) {
            set!(
                resource,
                urls::IS_A.to_owned(),
                Value::ResourceArray(vec![SubResource::Subject(Subject::from(class.as_str()))]),
            );
        }

        for (field, json) in &record.value {
            // JSON `null` means "absent" in Atomic Data — GitHub sends
            // `body: null` for a bodyless issue, and storing a placeholder
            // would make an empty body indistinguishable from an empty string.
            if json.is_null() {
                continue;
            }
            let (property, datatype) = self.property_for(&index, field);
            set!(
                resource,
                property,
                value_from_json(json, datatype.as_deref())
            );
        }

        // Required-property validation is off: the ontology is minted from an
        // OpenAPI document at run time, so a class's `requires` list is only
        // as complete as that document, and a partial record from the API
        // should still be stored rather than rejected.
        self.store
            .add_resource_opts(&resource, false, true, true)
            .await
            .map_err(|error| StorageError(error.to_string()))
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
            .map_err(|error| StorageError(error.to_string()))?;
        Ok(self.record_from_resource(&stored, &self.index()))
    }

    async fn list(&self, namespace: &str, resource: &str) -> Result<Vec<Record>, StorageError> {
        let prefix = self.record_prefix(namespace, resource);
        let index = self.index();
        let mut records: Vec<Record> = self
            .store
            .all_resources(false)
            .filter(|stored| stored.get_subject().to_string().starts_with(&prefix))
            .filter_map(|stored| self.record_from_resource(&stored, &index))
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
        self.store
            .remove_resource(&subject)
            .await
            .map_err(|error| StorageError(error.to_string()))
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
                        (subject.clone(), term.datatype.clone()),
                    );
                    properties.push(SubResource::Subject(Subject::from(subject.as_str())));
                }
            }
            let resource = self.term_resource(ontology, term)?;
            self.store
                .add_resource_opts(&resource, false, true, true)
                .await
                .map_err(|error| StorageError(error.to_string()))?;
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
        self.store
            .add_resource_opts(&ontology_resource, false, true, true)
            .await
            .map_err(|error| StorageError(error.to_string()))?;

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
fn value_from_json(json: &serde_json::Value, datatype: Option<&str>) -> Value {
    if let (Some(datatype), Some(text)) = (datatype, json.as_str()) {
        if let Ok(value) = Value::new(text, &match_datatype(datatype)) {
            return value;
        }
    }
    match json {
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(integer) => Value::Integer(integer),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        serde_json::Value::String(text) => Value::String(text.clone()),
        // Arrays and objects are kept verbatim rather than flattened into
        // nested resources: the ontology describes the API's own shapes, and
        // inventing subjects for anonymous sub-objects would mint terms the
        // document never declared.
        other => Value::Json(other.clone()),
    }
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
                    requires: vec!["github-issues/property/title".to_owned()],
                    recommends: vec!["github-issues/property/number".to_owned()],
                },
                OntologyTerm {
                    path: "github-issues/property/title".to_owned(),
                    kind: TermKind::Property,
                    shortname: "title".to_owned(),
                    description: "The issue's title.".to_owned(),
                    datatype: Some(urls::STRING.to_owned()),
                    requires: vec![],
                    recommends: vec![],
                },
                OntologyTerm {
                    path: "github-issues/property/number".to_owned(),
                    kind: TermKind::Property,
                    shortname: "number".to_owned(),
                    description: "Per-repository issue number.".to_owned(),
                    datatype: Some(urls::INTEGER.to_owned()),
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
