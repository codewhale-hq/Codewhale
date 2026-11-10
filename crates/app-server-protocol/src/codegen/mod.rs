use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

pub const MANIFEST_PATH: &str = "contract/codex-v2.json";
pub const PINNED_SCHEMA_PATH: &str = "tests/pinned/codex/codex_app_server_protocol.schemas.json";

pub struct Outputs {
    pub files: Vec<(&'static str, String)>,
}

pub fn generate(manifest: &str, pinned: &str) -> Result<Outputs, String> {
    let manifest: Value = serde_json::from_str(manifest).map_err(|error| error.to_string())?;
    let pinned: Value = serde_json::from_str(pinned).map_err(|error| error.to_string())?;
    let contract = Contract::load(&manifest, &pinned)?;
    let mut builder = Builder::new(&contract);
    for root in contract.roots()? {
        builder.def(&root)?;
    }
    for field in &contract.extension_fields {
        builder.def(&field.owner)?;
    }
    let items = builder.finish()?;
    let rust = RustOut::new(&items);
    Ok(Outputs {
        files: vec![
            ("src/methods.rs", methods_rs(&contract)?),
            ("src/v2.rs", rust.render(&contract)?),
            ("METHODS.md", methods_md(&contract, &items)),
            (
                "schema/json/codex-v2.schema.json",
                schema_json(&contract, &builder.raw)?,
            ),
            (
                "schema/typescript/codex-v2.d.ts",
                typescript(&contract, &items)?,
            ),
        ],
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    ClientRequest,
    ClientNotification,
    ServerRequest,
    ServerNotification,
}

impl Kind {
    const ALL: [Kind; 4] = [
        Kind::ClientRequest,
        Kind::ClientNotification,
        Kind::ServerRequest,
        Kind::ServerNotification,
    ];

    fn key(self) -> &'static str {
        match self {
            Kind::ClientRequest => "clientRequests",
            Kind::ClientNotification => "clientNotifications",
            Kind::ServerRequest => "serverRequests",
            Kind::ServerNotification => "serverNotifications",
        }
    }

    fn section(self) -> &'static str {
        match self {
            Kind::ClientRequest => "Client requests",
            Kind::ClientNotification => "Client notifications",
            Kind::ServerRequest => "Server requests",
            Kind::ServerNotification => "Server notifications",
        }
    }

    fn enum_name(self) -> &'static str {
        match self {
            Kind::ClientRequest => "ClientRequestMethod",
            Kind::ClientNotification => "ClientNotificationMethod",
            Kind::ServerRequest => "ServerRequestMethod",
            Kind::ServerNotification => "ServerNotificationMethod",
        }
    }

    fn table_name(self) -> &'static str {
        match self {
            Kind::ClientRequest => "CLIENT_REQUESTS",
            Kind::ClientNotification => "CLIENT_NOTIFICATIONS",
            Kind::ServerRequest => "SERVER_REQUESTS",
            Kind::ServerNotification => "SERVER_NOTIFICATIONS",
        }
    }

    fn union(self) -> &'static str {
        match self {
            Kind::ClientRequest => "ClientRequest",
            Kind::ClientNotification => "ClientNotification",
            Kind::ServerRequest => "ServerRequest",
            Kind::ServerNotification => "ServerNotification",
        }
    }
}

pub struct Row {
    pub method: String,
    pub tier: String,
    pub status: String,
    pub basis: String,
    pub recommended: Option<String>,
    pub variant: Option<String>,
    pub params: Option<String>,
    pub response: Option<String>,
}

impl Row {
    fn selected(&self) -> bool {
        matches!(self.tier.as_str(), "A" | "B")
    }
}

struct ExtensionField {
    owner: Key,
    field: String,
    schema: Value,
}

struct Contract<'a> {
    manifest: &'a Value,
    pinned: &'a Value,
    rows: BTreeMap<Kind, Vec<Row>>,
    extension_fields: Vec<ExtensionField>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Key {
    namespace: String,
    name: String,
}

impl Key {
    fn parse(reference: &str) -> Result<Key, String> {
        let path = reference
            .strip_prefix("#/definitions/")
            .ok_or_else(|| format!("unsupported $ref {reference}"))?;
        Ok(match path.split_once('/') {
            Some((namespace, name)) => Key {
                namespace: namespace.into(),
                name: name.into(),
            },
            None => Key {
                namespace: String::new(),
                name: path.into(),
            },
        })
    }

    fn reference(&self) -> String {
        if self.namespace.is_empty() {
            format!("#/definitions/{}", self.name)
        } else {
            format!("#/definitions/{}/{}", self.namespace, self.name)
        }
    }
}

fn text(value: &Value, field: &str) -> Result<String, String> {
    value[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("manifest field {field} missing in {value}"))
}

fn optional_text(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(str::to_owned)
}

impl<'a> Contract<'a> {
    fn load(manifest: &'a Value, pinned: &'a Value) -> Result<Self, String> {
        let mut rows = BTreeMap::new();
        for kind in Kind::ALL {
            let mut list = Vec::new();
            for row in manifest["methods"][kind.key()]
                .as_array()
                .ok_or_else(|| format!("manifest has no {}", kind.key()))?
            {
                list.push(Row {
                    method: text(row, "method")?,
                    tier: text(row, "tier")?,
                    status: text(row, "status")?,
                    basis: text(row, "basis")?,
                    recommended: optional_text(row, "recommended"),
                    variant: optional_text(row, "variant"),
                    params: optional_text(row, "params"),
                    response: optional_text(row, "response"),
                });
            }
            rows.insert(kind, list);
        }
        let mut extension_fields = Vec::new();
        for field in manifest["extensions"]["fields"]
            .as_array()
            .ok_or("manifest has no extension fields")?
        {
            let owner = text(field, "type")?;
            extension_fields.push(ExtensionField {
                owner: Key {
                    namespace: "v2".into(),
                    name: owner,
                },
                field: text(field, "field")?,
                schema: field["schema"].clone(),
            });
        }
        Ok(Contract {
            manifest,
            pinned,
            rows,
            extension_fields,
        })
    }

    fn schema(&self, key: &Key) -> Result<&'a Value, String> {
        let found = match key.namespace.as_str() {
            "" => self.pinned["definitions"].get(&key.name),
            "v2" => self.pinned["definitions"]["v2"].get(&key.name),
            "codewhale" => self.manifest["extensions"]["definitions"].get(&key.name),
            other => return Err(format!("unknown namespace {other}")),
        };
        found.ok_or_else(|| format!("no definition {}", key.reference()))
    }

    fn named(&self, name: &str) -> Result<Key, String> {
        for namespace in ["v2", ""] {
            let key = Key {
                namespace: namespace.into(),
                name: name.into(),
            };
            if self.schema(&key).is_ok() {
                return Ok(key);
            }
        }
        Err(format!("pinned export has no type {name}"))
    }

    fn union_params(&self, kind: Kind, method: &str) -> Result<Option<Key>, String> {
        let union = &self.pinned["definitions"][kind.union()];
        for variant in union["oneOf"]
            .as_array()
            .ok_or_else(|| format!("pinned {} is not a union", kind.union()))?
        {
            if variant["properties"]["method"]["enum"][0] == method {
                return match variant["properties"]["params"]["$ref"].as_str() {
                    Some(reference) => Ok(Some(Key::parse(reference)?)),
                    None => Ok(None),
                };
            }
        }
        Err(format!("pinned {} has no {method}", kind.union()))
    }

    fn selected(&self, kind: Kind) -> impl Iterator<Item = &Row> {
        self.rows[&kind].iter().filter(|row| row.selected())
    }

    fn params_key(&self, kind: Kind, row: &Row) -> Result<Option<Key>, String> {
        let key = self.union_params(kind, &row.method)?;
        match (&key, &row.params) {
            (Some(key), Some(name)) if &key.name == name => Ok(Some(key.clone())),
            (None, None) => Ok(None),
            _ => Err(format!(
                "{}: manifest params {:?} disagree with the pinned union {:?}",
                row.method, row.params, key
            )),
        }
    }

    fn response_key(&self, row: &Row) -> Result<Key, String> {
        self.named(
            row.response
                .as_deref()
                .ok_or_else(|| format!("{} has no response type", row.method))?,
        )
    }

    fn roots(&self) -> Result<Vec<Key>, String> {
        let mut roots = Vec::new();
        for kind in Kind::ALL {
            for row in self.selected(kind) {
                if row.variant.is_none() {
                    return Err(format!("{} has no variant name", row.method));
                }
                if let Some(key) = self.params_key(kind, row)? {
                    roots.push(key);
                }
                if matches!(kind, Kind::ClientRequest | Kind::ServerRequest) {
                    roots.push(self.response_key(row)?);
                }
            }
        }
        if let Some(definitions) = self.manifest["extensions"]["definitions"].as_object() {
            for name in definitions.keys() {
                if self.named(name).is_ok() {
                    return Err(format!(
                        "Codewhale extension {name} shadows a pinned Codex type"
                    ));
                }
                roots.push(Key {
                    namespace: "codewhale".into(),
                    name: name.clone(),
                });
            }
        }
        Ok(roots)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    String,
    Bool,
    Int(&'static str),
    Json,
    JsonObject,
    Named(String),
    Vec(Box<Ty>),
    Map(Box<Ty>),
    NullableMap(Box<Ty>),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub wire: String,
    pub ty: Ty,
    pub nullable: bool,
    pub required: bool,
    pub default: Option<Value>,
    pub flatten: bool,
}

#[derive(Clone, Debug)]
pub enum External {
    Unit(String),
    Newtype(String, Ty),
    Struct(String, Vec<Field>),
}

#[derive(Clone, Debug)]
pub enum Shape {
    Alias(Ty),
    Struct {
        fields: Vec<Field>,
        deny_unknown: bool,
        additional: bool,
    },
    StringEnum(Vec<String>),
    Tagged {
        tag: String,
        variants: Vec<(String, Vec<Field>)>,
    },
    External(Vec<External>),
    Untagged(Vec<(String, Ty)>),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub shape: Shape,
}

struct Builder<'a> {
    contract: &'a Contract<'a>,
    items: BTreeMap<String, Item>,
    origin: BTreeMap<String, Key>,
    raw: BTreeMap<String, Value>,
}

fn stripped(schema: &Value) -> Value {
    match schema {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "description" | "title" | "$schema"))
                .map(|(key, value)| (key.clone(), stripped(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(stripped).collect()),
        other => other.clone(),
    }
}

fn types_of(schema: &Value) -> (Vec<String>, bool) {
    match &schema["type"] {
        Value::String(single) => (vec![single.clone()], false),
        Value::Array(list) => {
            let names: Vec<String> = list
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| *name != "null")
                .map(str::to_owned)
                .collect();
            let nullable = list.iter().any(|name| name == "null");
            (names, nullable)
        }
        _ => (Vec::new(), false),
    }
}

fn is_null_schema(schema: &Value) -> bool {
    schema["type"] == "null"
}

fn single_tag(schema: &Value) -> Option<(String, String)> {
    let properties = schema["properties"].as_object()?;
    let mut found = None;
    for (name, property) in properties {
        let values = property["enum"].as_array();
        if property["type"] == "string" && values.is_some_and(|values| values.len() == 1) {
            if found.is_some() {
                return None;
            }
            found = Some((name.clone(), values?[0].as_str()?.to_owned()));
        }
    }
    found
}

fn string_enum_values(schema: &Value) -> Option<Vec<String>> {
    if schema["type"] != "string" {
        return None;
    }
    schema["enum"]
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn single_key_object(schema: &Value) -> Option<(String, &Value)> {
    let properties = schema["properties"].as_object()?;
    if properties.len() != 1 {
        return None;
    }
    let (name, property) = properties.iter().next()?;
    let required = schema["required"].as_array()?;
    if required.len() == 1 && required[0] == name.as_str() {
        Some((name.clone(), property))
    } else {
        None
    }
}

impl<'a> Builder<'a> {
    fn new(contract: &'a Contract<'a>) -> Self {
        Builder {
            contract,
            items: BTreeMap::new(),
            origin: BTreeMap::new(),
            raw: BTreeMap::new(),
        }
    }

    fn def(&mut self, key: &Key) -> Result<String, String> {
        let name = key.name.clone();
        if let Some(previous) = self.origin.get(&name) {
            if previous == key {
                return Ok(name);
            }
            let before = stripped(self.contract.schema(previous)?);
            let after = stripped(self.contract.schema(key)?);
            if before == after {
                return Ok(name);
            }
            return Err(format!(
                "{} and {} share a name with different shapes",
                previous.reference(),
                key.reference()
            ));
        }
        self.origin.insert(name.clone(), key.clone());
        let schema = self.contract.schema(key)?;
        self.raw.insert(name.clone(), schema.clone());
        let shape = self.shape(&name, schema, &key.namespace)?;
        self.items.insert(
            name.clone(),
            Item {
                name: name.clone(),
                shape,
            },
        );
        Ok(name)
    }

    fn inline(&mut self, name: String, schema: &Value, namespace: &str) -> Result<String, String> {
        if self.items.contains_key(&name) || self.origin.contains_key(&name) {
            return Err(format!("inline type {name} collides with a definition"));
        }
        self.origin.insert(
            name.clone(),
            Key {
                namespace: "inline".into(),
                name: name.clone(),
            },
        );
        let shape = self.shape(&name, schema, namespace)?;
        self.items.insert(
            name.clone(),
            Item {
                name: name.clone(),
                shape,
            },
        );
        Ok(name)
    }

    fn reference(&mut self, reference: &str, namespace: &str) -> Result<String, String> {
        let mut key = Key::parse(reference)?;
        if key.namespace == "inline" {
            key.namespace = namespace.into();
        }
        self.def(&key)
    }

    fn ty(&mut self, context: &str, schema: &Value, namespace: &str) -> Result<(Ty, bool), String> {
        if schema == &Value::Bool(true) {
            return Ok((Ty::Json, false));
        }
        let object = schema
            .as_object()
            .ok_or_else(|| format!("{context}: unsupported schema {schema}"))?;
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            return Ok((Ty::Named(self.reference(reference, namespace)?), false));
        }
        if let Some(all) = object.get("allOf").and_then(Value::as_array) {
            if all.len() != 1 {
                return Err(format!("{context}: allOf with {} members", all.len()));
            }
            return self.ty(context, &all[0], namespace);
        }
        for union in ["anyOf", "oneOf"] {
            if let Some(members) = object.get(union).and_then(Value::as_array) {
                let present: Vec<&Value> = members
                    .iter()
                    .filter(|member| !is_null_schema(member))
                    .collect();
                let nullable = present.len() != members.len();
                if present.len() == 1 {
                    let (ty, inner) = self.ty(context, present[0], namespace)?;
                    return Ok((ty, nullable || inner));
                }
                let mut copy = object.clone();
                copy.insert(
                    union.into(),
                    Value::Array(present.into_iter().cloned().collect()),
                );
                let name = self.inline(context.into(), &Value::Object(copy), namespace)?;
                return Ok((Ty::Named(name), nullable));
            }
        }
        let (types, nullable) = types_of(schema);
        if types.is_empty() {
            if object
                .keys()
                .all(|key| matches!(key.as_str(), "description" | "title" | "default"))
            {
                return Ok((Ty::Json, false));
            }
            return Err(format!("{context}: untyped schema {schema}"));
        }
        if types.len() != 1 {
            return Ok((Ty::Json, nullable));
        }
        let ty = match types[0].as_str() {
            "string" => {
                if object.contains_key("enum") {
                    Ty::Named(self.inline(context.into(), &without_null(schema), namespace)?)
                } else {
                    Ty::String
                }
            }
            "boolean" => Ty::Bool,
            "integer" => Ty::Int(match object.get("format").and_then(Value::as_str) {
                None | Some("int64") => "i64",
                Some("int32") => "i32",
                Some("uint8") => "u8",
                Some("uint16") => "u16",
                Some("uint32") => "u32",
                Some("uint64") | Some("uint") => "u64",
                Some(other) => return Err(format!("{context}: integer format {other}")),
            }),
            "array" => {
                let items = object
                    .get("items")
                    .ok_or_else(|| format!("{context}: array without items"))?;
                let (item, item_nullable) = self.ty(&format!("{context}Item"), items, namespace)?;
                if item_nullable {
                    return Err(format!("{context}: nullable array items"));
                }
                Ty::Vec(Box::new(item))
            }
            "object" => {
                if object.contains_key("properties") {
                    Ty::Named(self.inline(context.into(), &without_null(schema), namespace)?)
                } else {
                    match object.get("additionalProperties") {
                        Some(Value::Object(_)) => {
                            let (value, value_nullable) = self.ty(
                                &format!("{context}Value"),
                                &object["additionalProperties"],
                                namespace,
                            )?;
                            if value_nullable {
                                Ty::NullableMap(Box::new(value))
                            } else {
                                Ty::Map(Box::new(value))
                            }
                        }
                        _ => Ty::JsonObject,
                    }
                }
            }
            other => return Err(format!("{context}: type {other}")),
        };
        Ok((ty, nullable))
    }

    fn fields(
        &mut self,
        owner: &str,
        schema: &Value,
        namespace: &str,
        skip: Option<&str>,
    ) -> Result<Vec<Field>, String> {
        let required: BTreeSet<String> = schema["required"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut fields = Vec::new();
        if let Some(properties) = schema["properties"].as_object() {
            for (wire, property) in properties {
                if Some(wire.as_str()) == skip {
                    continue;
                }
                let context = format!("{owner}{}", pascal(wire));
                let (ty, nullable) = self.ty(&context, property, namespace)?;
                let default = property
                    .get("default")
                    .filter(|value| !value.is_null())
                    .cloned();
                fields.push(Field {
                    wire: wire.clone(),
                    ty,
                    nullable,
                    required: required.contains(wire),
                    default,
                    flatten: false,
                });
            }
        }
        if schema.get("properties").is_some() {
            for union in ["oneOf", "anyOf"] {
                if let Some(members) = schema[union].as_array() {
                    let name = format!("{owner}Kind");
                    if self.origin.contains_key(&name) {
                        return Err(format!("flattened union {name} collides with a definition"));
                    }
                    self.origin.insert(
                        name.clone(),
                        Key {
                            namespace: "inline".into(),
                            name: name.clone(),
                        },
                    );
                    let shape = self.union(&name, members, namespace)?;
                    self.items.insert(
                        name.clone(),
                        Item {
                            name: name.clone(),
                            shape,
                        },
                    );
                    fields.push(Field {
                        wire: "kind".into(),
                        ty: Ty::Named(name),
                        nullable: false,
                        required: true,
                        default: None,
                        flatten: true,
                    });
                }
            }
        }
        for name in &required {
            if Some(name.as_str()) != skip && !fields.iter().any(|field| &field.wire == name) {
                return Err(format!("{owner}: required {name} has no property"));
            }
        }
        Ok(fields)
    }

    fn shape(&mut self, name: &str, schema: &Value, namespace: &str) -> Result<Shape, String> {
        for union in ["oneOf", "anyOf"] {
            if let Some(members) = schema[union].as_array()
                && schema.get("properties").is_none()
            {
                return self.union(name, members, namespace);
            }
        }
        if let Some(values) = string_enum_values(schema) {
            return Ok(Shape::StringEnum(values));
        }
        let (types, _) = types_of(schema);
        if types.len() == 1 && types[0] == "object" && schema.get("properties").is_some() {
            let mut fields = self.fields(name, schema, namespace, None)?;
            for extension in &self.contract.extension_fields {
                if extension.owner.name == name {
                    let (ty, nullable) = self.ty(
                        &format!("{name}{}", pascal(&extension.field)),
                        &extension.schema,
                        namespace,
                    )?;
                    fields.push(Field {
                        wire: extension.field.clone(),
                        ty,
                        nullable,
                        required: false,
                        default: None,
                        flatten: false,
                    });
                }
            }
            let flattened = fields.iter().any(|field| field.flatten);
            return Ok(Shape::Struct {
                fields,
                deny_unknown: schema["additionalProperties"] == false && !flattened,
                additional: schema["additionalProperties"] == true,
            });
        }
        if types.len() == 1 && types[0] == "object" && schema.get("additionalProperties").is_none()
        {
            return Ok(Shape::Struct {
                fields: Vec::new(),
                deny_unknown: false,
                additional: false,
            });
        }
        let (ty, nullable) = self.ty(&format!("{name}Inner"), schema, namespace)?;
        if nullable {
            return Err(format!("{name}: nullable alias"));
        }
        Ok(Shape::Alias(ty))
    }

    fn union(&mut self, name: &str, members: &[Value], namespace: &str) -> Result<Shape, String> {
        if members
            .iter()
            .all(|member| string_enum_values(member).is_some())
        {
            let mut values = Vec::new();
            for member in members {
                values.extend(string_enum_values(member).unwrap_or_default());
            }
            return Ok(Shape::StringEnum(values));
        }
        let tags: Vec<Option<(String, String)>> = members.iter().map(single_tag).collect();
        if let Some(Some((tag, _))) = tags.first()
            && tags
                .iter()
                .all(|found| found.as_ref().is_some_and(|(name, _)| name == tag))
        {
            let mut variants = Vec::new();
            for (member, found) in members.iter().zip(&tags) {
                let (_, value) = found.clone().unwrap_or_default();
                let owner = format!("{name}{}", pascal(&value));
                let fields = self.fields(&owner, member, namespace, Some(tag))?;
                variants.push((value, fields));
            }
            return Ok(Shape::Tagged {
                tag: tag.clone(),
                variants,
            });
        }
        if members.iter().all(|member| {
            string_enum_values(member).is_some() || single_key_object(member).is_some()
        }) {
            let mut variants = Vec::new();
            for member in members {
                if let Some(values) = string_enum_values(member) {
                    variants.extend(values.into_iter().map(External::Unit));
                    continue;
                }
                let (key, payload) = single_key_object(member).ok_or("external variant")?;
                let owner = format!("{name}{}", pascal(&key));
                if payload["type"] == "object" && payload.get("properties").is_some() {
                    let fields = self.fields(&owner, payload, namespace, None)?;
                    variants.push(External::Struct(key, fields));
                } else {
                    let (ty, nullable) = self.ty(&owner, payload, namespace)?;
                    if nullable {
                        return Err(format!("{owner}: nullable external payload"));
                    }
                    variants.push(External::Newtype(key, ty));
                }
            }
            return Ok(Shape::External(variants));
        }
        let mut variants = Vec::new();
        for (index, member) in members.iter().enumerate() {
            let (ty, nullable) = self.ty(&format!("{name}Variant{index}"), member, namespace)?;
            if nullable {
                return Err(format!("{name}: nullable untagged member"));
            }
            let titled = member["title"]
                .as_str()
                .map(|title| title.strip_suffix(name).unwrap_or(title).to_owned())
                .filter(|title| !title.is_empty());
            let label = match &ty {
                _ if titled.is_some() => titled.clone().unwrap_or_default(),
                Ty::Json => "Other".to_owned(),
                Ty::String => "String".to_owned(),
                Ty::Int(_) => "Integer".to_owned(),
                Ty::Bool => "Bool".to_owned(),
                Ty::Vec(item) => match item.as_ref() {
                    Ty::String => "Strings".to_owned(),
                    Ty::Named(inner) => format!("{inner}List"),
                    _ => "List".to_owned(),
                },
                Ty::Named(inner) => inner.clone(),
                _ => format!("Variant{index}"),
            };
            if variants.iter().any(|(existing, _)| existing == &label) {
                return Err(format!("{name}: duplicate untagged label {label}"));
            }
            variants.push((label, ty));
        }
        Ok(Shape::Untagged(variants))
    }

    fn finish(&self) -> Result<Vec<Item>, String> {
        Ok(self.items.values().cloned().collect())
    }
}

fn without_null(schema: &Value) -> Value {
    let mut copy = schema.clone();
    if let Some(Value::Array(list)) = copy.get("type").cloned() {
        let present: Vec<Value> = list.into_iter().filter(|name| name != "null").collect();
        if present.len() == 1 {
            copy["type"] = present[0].clone();
        }
    }
    copy
}

pub fn pascal(value: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if upper {
                out.extend(character.to_uppercase());
                upper = false;
            } else {
                out.push(character);
            }
        } else {
            upper = true;
        }
    }
    if out.starts_with(|character: char| character.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    out
}

pub fn snake(value: &str) -> String {
    let mut out = String::new();
    let characters: Vec<char> = value.chars().collect();
    for (index, character) in characters.iter().enumerate() {
        if character.is_ascii_uppercase() {
            let previous_lower = index > 0
                && (characters[index - 1].is_ascii_lowercase()
                    || characters[index - 1].is_ascii_digit());
            let next_lower = characters
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_lowercase());
            let previous_upper = index > 0 && characters[index - 1].is_ascii_uppercase();
            if previous_lower || (previous_upper && next_lower) {
                out.push('_');
            }
            out.push(character.to_ascii_lowercase());
        } else if character.is_ascii_alphanumeric() {
            out.push(*character);
        } else {
            out.push('_');
        }
    }
    out
}

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

fn ident(wire: &str) -> String {
    let name = snake(wire);
    if KEYWORDS.contains(&name.as_str()) {
        format!("r#{name}")
    } else {
        name
    }
}

struct RustOut<'a> {
    items: &'a [Item],
    boxed: BTreeSet<(String, String)>,
}

fn direct_refs(ty: &Ty, out: &mut Vec<String>) {
    if let Ty::Named(name) = ty {
        out.push(name.clone());
    }
}

fn item_edges(item: &Item) -> Vec<String> {
    fn fields(list: &[Field], out: &mut Vec<String>) {
        for field in list {
            direct_refs(&field.ty, out);
        }
    }
    let mut out = Vec::new();
    match &item.shape {
        Shape::Struct { fields: list, .. } => fields(list, &mut out),
        Shape::Tagged { variants, .. } => {
            for (_, list) in variants {
                fields(list, &mut out);
            }
        }
        Shape::External(variants) => {
            for variant in variants {
                match variant {
                    External::Unit(_) => {}
                    External::Newtype(_, ty) => direct_refs(ty, &mut out),
                    External::Struct(_, list) => fields(list, &mut out),
                }
            }
        }
        Shape::Untagged(variants) => {
            for (_, ty) in variants {
                direct_refs(ty, &mut out);
            }
        }
        Shape::Alias(ty) => direct_refs(ty, &mut out),
        Shape::StringEnum(_) => {}
    }
    out
}

fn strongly_connected(items: &[Item]) -> BTreeMap<String, usize> {
    let edges: BTreeMap<String, Vec<String>> = items
        .iter()
        .map(|item| (item.name.clone(), item_edges(item)))
        .collect();
    struct State<'a> {
        edges: &'a BTreeMap<String, Vec<String>>,
        index: BTreeMap<String, usize>,
        low: BTreeMap<String, usize>,
        stack: Vec<String>,
        on_stack: BTreeSet<String>,
        next: usize,
        component: BTreeMap<String, usize>,
        components: usize,
    }
    fn visit(state: &mut State<'_>, node: &str) {
        state.index.insert(node.into(), state.next);
        state.low.insert(node.into(), state.next);
        state.next += 1;
        state.stack.push(node.into());
        state.on_stack.insert(node.into());
        let targets = state.edges.get(node).cloned().unwrap_or_default();
        for target in targets {
            if !state.edges.contains_key(&target) {
                continue;
            }
            if !state.index.contains_key(&target) {
                visit(state, &target);
                let low = state.low[node].min(state.low[&target]);
                state.low.insert(node.into(), low);
            } else if state.on_stack.contains(&target) {
                let low = state.low[node].min(state.index[&target]);
                state.low.insert(node.into(), low);
            }
        }
        if state.low[node] == state.index[node] {
            while let Some(member) = state.stack.pop() {
                state.on_stack.remove(&member);
                state.component.insert(member.clone(), state.components);
                if member == node {
                    break;
                }
            }
            state.components += 1;
        }
    }
    let mut state = State {
        edges: &edges,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        next: 0,
        component: BTreeMap::new(),
        components: 0,
    };
    for name in edges.keys() {
        if !state.index.contains_key(name) {
            visit(&mut state, name);
        }
    }
    state.component
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

impl<'a> RustOut<'a> {
    fn new(items: &'a [Item]) -> Self {
        let component = strongly_connected(items);
        let mut boxed = BTreeSet::new();
        for item in items {
            for target in item_edges(item) {
                if component.contains_key(&item.name)
                    && component.get(&item.name) == component.get(&target)
                {
                    boxed.insert((item.name.clone(), target));
                }
            }
        }
        RustOut { items, boxed }
    }

    fn ty(&self, owner: &str, ty: &Ty) -> String {
        match ty {
            Ty::String => "String".into(),
            Ty::Bool => "bool".into(),
            Ty::Int(name) => (*name).into(),
            Ty::Json => "Value".into(),
            Ty::JsonObject => "Map<String, Value>".into(),
            Ty::Named(name) => {
                if self.boxed.contains(&(owner.to_owned(), name.clone())) {
                    format!("Box<{name}>")
                } else {
                    name.clone()
                }
            }
            Ty::Vec(item) => format!("Vec<{}>", self.ty("", item)),
            Ty::Map(value) => format!("BTreeMap<String, {}>", self.ty("", value)),
            Ty::NullableMap(value) => format!("BTreeMap<String, Option<{}>>", self.ty("", value)),
        }
    }

    fn trivial_default(value: &Value) -> bool {
        matches!(value, Value::Bool(false))
            || value.as_i64() == Some(0)
            || value.as_array().is_some_and(Vec::is_empty)
    }

    fn literal(&self, owner: &str, ty: &Ty, value: &Value) -> Result<String, String> {
        Ok(match (ty, value) {
            (Ty::Bool, Value::Bool(flag)) => flag.to_string(),
            (Ty::Int(_), Value::Number(number)) => number.to_string(),
            (Ty::String, Value::String(text)) => format!("{}.to_owned()", quote(text)),
            (Ty::Vec(item), Value::Array(values)) => {
                let parts: Result<Vec<String>, String> = values
                    .iter()
                    .map(|value| self.literal("", item, value))
                    .collect();
                format!("vec![{}]", parts?.join(", "))
            }
            (Ty::Named(name), Value::String(text)) => {
                let item = self
                    .items
                    .iter()
                    .find(|item| &item.name == name)
                    .ok_or_else(|| format!("default names unknown type {name}"))?;
                match &item.shape {
                    Shape::StringEnum(values) if values.contains(text) => {
                        let built = format!("{name}::{}", pascal(text));
                        if self.boxed.contains(&(owner.to_owned(), name.clone())) {
                            format!("Box::new({built})")
                        } else {
                            built
                        }
                    }
                    Shape::External(variants)
                        if variants.iter().any(
                            |variant| matches!(variant, External::Unit(unit) if unit == text),
                        ) =>
                    {
                        format!("{name}::{}", pascal(text))
                    }
                    _ => return Err(format!("default {text} does not name a {name} variant")),
                }
            }
            _ => return Err(format!("unsupported default {value} for {ty:?}")),
        })
    }

    fn fields(
        &self,
        owner: &str,
        fields: &[Field],
        visibility: &str,
        indent: &str,
        defaults: &mut String,
    ) -> Result<String, String> {
        let mut out = String::new();
        let mut seen = BTreeSet::new();
        for field in fields {
            let name = ident(&field.wire);
            if !seen.insert(name.clone()) {
                return Err(format!("{owner}: field {name} twice"));
            }
            let base = self.ty(owner, &field.ty);
            if field.flatten {
                let _ = writeln!(
                    out,
                    "{indent}#[serde(flatten)]\n{indent}{visibility}{name}: {base},"
                );
                continue;
            }
            let mut attributes = Vec::new();
            if name.trim_start_matches("r#") != field.wire {
                attributes.push(format!("rename = {}", quote(&field.wire)));
            }
            let ty = match (&field.default, field.required) {
                (Some(default), false) => {
                    let ty = if field.nullable {
                        format!("Option<{base}>")
                    } else {
                        base.clone()
                    };
                    if !field.nullable && Self::trivial_default(default) {
                        attributes.push("default".into());
                    } else {
                        let mut function =
                            format!("default_{}_{}", snake(owner), snake(&field.wire));
                        while defaults.contains(&format!("fn {function}()")) {
                            function.push('_');
                        }
                        let literal = self.literal(owner, &field.ty, default)?;
                        let body = if field.nullable {
                            format!("Some({literal})")
                        } else {
                            literal
                        };
                        let _ = write!(defaults, "fn {function}() -> {ty} {{\n    {body}\n}}\n\n");
                        attributes.push(format!("default = {}", quote(&function)));
                    }
                    ty
                }
                (_, true) => {
                    if field.nullable {
                        format!("Option<{base}>")
                    } else {
                        base
                    }
                }
                (None, false) => {
                    attributes.push("default".into());
                    attributes.push("skip_serializing_if = \"Option::is_none\"".into());
                    format!("Option<{base}>")
                }
            };
            if !attributes.is_empty() {
                let _ = writeln!(out, "{indent}#[serde({})]", attributes.join(", "));
            }
            let _ = writeln!(out, "{indent}{visibility}{name}: {ty},");
        }
        Ok(out)
    }

    fn derives(&self, item: &Item) -> String {
        let mut derives = vec!["Debug", "Clone", "PartialEq", "Eq"];
        match &item.shape {
            Shape::StringEnum(_) => derives.extend(["Copy", "Hash", "PartialOrd", "Ord"]),
            Shape::Struct { fields, .. }
                if fields
                    .iter()
                    .all(|field| !field.required && field.default.is_none()) =>
            {
                derives.push("Default")
            }
            _ => {}
        }
        derives.extend(["Serialize", "Deserialize"]);
        format!(
            "#[derive({})]\n#[cfg_attr(feature = \"schema\", derive(schemars::JsonSchema))]\n",
            derives.join(", ")
        )
    }

    fn item(&self, item: &Item, defaults: &mut String) -> Result<String, String> {
        let name = &item.name;
        let mut out = String::new();
        match &item.shape {
            Shape::Alias(ty) => {
                let _ = writeln!(out, "pub type {name} = {};", self.ty(name, ty));
            }
            Shape::Struct {
                fields,
                deny_unknown,
                additional,
            } => {
                out.push_str(&self.derives(item));
                if *deny_unknown {
                    out.push_str("#[serde(deny_unknown_fields)]\n");
                }
                let _ = writeln!(out, "pub struct {name} {{");
                out.push_str(&self.fields(name, fields, "pub ", "    ", defaults)?);
                if *additional {
                    out.push_str(
                        "    #[serde(flatten)]\n    pub additional: Map<String, Value>,\n",
                    );
                }
                out.push_str("}\n");
            }
            Shape::StringEnum(values) => {
                out.push_str(&self.derives(item));
                let _ = writeln!(out, "pub enum {name} {{");
                let mut seen = BTreeSet::new();
                for value in values {
                    let variant = pascal(value);
                    if !seen.insert(variant.clone()) {
                        return Err(format!("{name}: variant {variant} twice"));
                    }
                    let _ = writeln!(
                        out,
                        "    #[serde(rename = {})]\n    {variant},",
                        quote(value)
                    );
                }
                out.push_str("}\n");
            }
            Shape::Tagged { tag, variants } => {
                out.push_str(&self.derives(item));
                let _ = writeln!(out, "#[serde(tag = {})]", quote(tag));
                let _ = writeln!(out, "pub enum {name} {{");
                let mut seen = BTreeSet::new();
                for (value, fields) in variants {
                    let variant = pascal(value);
                    if !seen.insert(variant.clone()) {
                        return Err(format!("{name}: variant {variant} twice"));
                    }
                    let _ = writeln!(out, "    #[serde(rename = {})]", quote(value));
                    if fields.is_empty() {
                        let _ = writeln!(out, "    {variant} {{}},");
                    } else {
                        let _ = writeln!(out, "    {variant} {{");
                        out.push_str(&self.fields(name, fields, "", "        ", defaults)?);
                        out.push_str("    },\n");
                    }
                }
                out.push_str("}\n");
            }
            Shape::External(variants) => {
                out.push_str(&self.derives(item));
                let _ = writeln!(out, "pub enum {name} {{");
                let mut seen = BTreeSet::new();
                for variant in variants {
                    let (wire, body) = match variant {
                        External::Unit(wire) => (wire, String::new()),
                        External::Newtype(wire, ty) => (wire, format!("({})", self.ty(name, ty))),
                        External::Struct(wire, fields) => (
                            wire,
                            format!(
                                " {{\n{}    }}",
                                self.fields(name, fields, "", "        ", defaults)?
                            ),
                        ),
                    };
                    let label = pascal(wire);
                    if !seen.insert(label.clone()) {
                        return Err(format!("{name}: variant {label} twice"));
                    }
                    let _ = writeln!(
                        out,
                        "    #[serde(rename = {})]\n    {label}{body},",
                        quote(wire)
                    );
                }
                out.push_str("}\n");
            }
            Shape::Untagged(variants) => {
                out.push_str(&self.derives(item));
                out.push_str("#[serde(untagged)]\n");
                let _ = writeln!(out, "pub enum {name} {{");
                for (label, ty) in variants {
                    let _ = writeln!(out, "    {label}({}),", self.ty(name, ty));
                }
                out.push_str("}\n");
            }
        }
        Ok(out)
    }

    fn render(&self, contract: &Contract<'_>) -> Result<String, String> {
        let mut out = String::new();
        out.push_str("#![allow(clippy::large_enum_variant, clippy::enum_variant_names)]\n\n");
        out.push_str("use serde::{Deserialize, Serialize};\nuse serde_json::{Map, Value};\nuse std::collections::BTreeMap;\n\n");
        out.push_str("pub use crate::methods::{\n    ClientNotificationMethod, ClientRequestMethod, ServerNotificationMethod, ServerRequestMethod,\n};\n\n");
        let mut defaults = String::new();
        for item in self.items {
            out.push_str(&self.item(item, &mut defaults)?);
            out.push('\n');
        }
        out.push_str(&defaults);
        out.push_str(&envelopes(contract)?);
        out.push_str("pub const TYPE_NAMES: &[&str] = &[\n");
        for item in self.items {
            let _ = writeln!(out, "    {},", quote(&item.name));
        }
        out.push_str("];\n\n");
        out.push_str("pub fn round_trip(type_name: &str, value: Value) -> Option<Result<Value, serde_json::Error>> {\n    fn again<T: serde::de::DeserializeOwned + Serialize>(value: Value) -> Result<Value, serde_json::Error> {\n        serde_json::to_value(serde_json::from_value::<T>(value)?)\n    }\n    Some(match type_name {\n");
        for item in self.items {
            let _ = writeln!(
                out,
                "        {} => again::<{}>(value),",
                quote(&item.name),
                item.name
            );
        }
        out.push_str("        _ => return None,\n    })\n}\n");
        Ok(out)
    }
}

fn envelopes(contract: &Contract<'_>) -> Result<String, String> {
    let mut out = String::new();
    let enum_of = |out: &mut String,
                   name: &str,
                   kind: Kind,
                   payload: &dyn Fn(&Row) -> Result<Option<String>, String>|
     -> Result<(), String> {
        let _ = writeln!(
            out,
            "#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(tag = \"method\", content = \"params\")]\npub enum {name} {{"
        );
        for row in contract.selected(kind) {
            let variant = row.variant.clone().unwrap_or_default();
            let _ = writeln!(out, "    #[serde(rename = {})]", quote(&row.method));
            match payload(row)? {
                Some(ty) => {
                    let _ = writeln!(out, "    {variant}({ty}),");
                }
                None => {
                    let _ = writeln!(out, "    {variant},");
                }
            }
        }
        out.push_str("}\n\n");
        let method_enum = kind.enum_name();
        let _ = writeln!(
            out,
            "impl {name} {{\n    pub fn method(&self) -> {method_enum} {{\n        match self {{"
        );
        for row in contract.selected(kind) {
            let variant = row.variant.clone().unwrap_or_default();
            let pattern = if payload(row)?.is_some() { "(_)" } else { "" };
            let _ = writeln!(
                out,
                "            Self::{variant}{pattern} => {method_enum}::{variant},"
            );
        }
        out.push_str("        }\n    }\n}\n\n");
        Ok(())
    };
    enum_of(
        &mut out,
        "ClientRequestParams",
        Kind::ClientRequest,
        &|row| Ok(row.params.clone()),
    )?;
    enum_of(
        &mut out,
        "ClientNotification",
        Kind::ClientNotification,
        &|row| Ok(row.params.clone()),
    )?;
    enum_of(
        &mut out,
        "ServerRequestParams",
        Kind::ServerRequest,
        &|row| Ok(row.params.clone()),
    )?;
    enum_of(
        &mut out,
        "ServerNotification",
        Kind::ServerNotification,
        &|row| Ok(row.params.clone()),
    )?;
    for (name, kind) in [
        ("ClientResponse", Kind::ClientRequest),
        ("ServerRequestResponse", Kind::ServerRequest),
    ] {
        let method_enum = kind.enum_name();
        let _ = writeln!(
            out,
            "#[derive(Debug, Clone, PartialEq, Eq, Serialize)]\n#[serde(untagged)]\npub enum {name} {{"
        );
        for row in contract.selected(kind) {
            let _ = writeln!(
                out,
                "    {}({}),",
                row.variant.clone().unwrap_or_default(),
                row.response.clone().unwrap_or_default()
            );
        }
        let _ = writeln!(
            out,
            "}}\n\nimpl {name} {{\n    pub fn method(&self) -> {method_enum} {{\n        match self {{"
        );
        for row in contract.selected(kind) {
            let variant = row.variant.clone().unwrap_or_default();
            let _ = writeln!(
                out,
                "            Self::{variant}(_) => {method_enum}::{variant},"
            );
        }
        let _ = writeln!(
            out,
            "        }}\n    }}\n\n    pub fn decode(method: {method_enum}, result: Value) -> Result<Self, serde_json::Error> {{\n        Ok(match method {{"
        );
        for row in contract.selected(kind) {
            let variant = row.variant.clone().unwrap_or_default();
            let _ = writeln!(
                out,
                "            {method_enum}::{variant} => Self::{variant}(serde_json::from_value(result)?),"
            );
        }
        out.push_str("        })\n    }\n}\n\n");
    }
    Ok(out)
}

fn methods_rs(contract: &Contract<'_>) -> Result<String, String> {
    let manifest = contract.manifest;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "pub const CONTRACT_NAME: &str = {};\npub const CONTRACT_VERSION: u32 = {};\npub const CODEX_PROTOCOL_TAG: &str = {};\npub const CODEX_PROTOCOL_COMMIT: &str = {};\n",
        quote(manifest["contract"].as_str().unwrap_or_default()),
        manifest["version"].as_u64().ok_or("manifest version")?,
        quote(manifest["upstream"]["tag"].as_str().unwrap_or_default()),
        quote(manifest["upstream"]["commit"].as_str().unwrap_or_default()),
    );
    out.push_str("pub mod error_code {\n");
    for error in manifest["errors"].as_array().ok_or("manifest errors")? {
        let _ = writeln!(
            out,
            "    pub const {}: i64 = {};",
            error["name"].as_str().ok_or("error name")?,
            error["code"].as_i64().ok_or("error code")?
        );
    }
    out.push_str("}\n\n");
    out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub enum Tier {\n    A,\n    B,\n}\n\n");
    out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub enum Disposition {\n    Selected(Tier),\n    C,\n    Undecided,\n}\n\nimpl Disposition {\n    pub const fn selected(self) -> bool {\n        matches!(self, Self::Selected(_))\n    }\n}\n\n");
    for kind in Kind::ALL {
        let name = kind.enum_name();
        let rows: Vec<&Row> = contract.selected(kind).collect();
        let _ = writeln!(
            out,
            "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\npub enum {name} {{"
        );
        for row in &rows {
            let _ = writeln!(out, "    {},", row.variant.clone().unwrap_or_default());
        }
        let _ = writeln!(
            out,
            "}}\n\nimpl {name} {{\n    pub const ALL: &'static [Self] = &["
        );
        for row in &rows {
            let _ = writeln!(
                out,
                "        Self::{},",
                row.variant.clone().unwrap_or_default()
            );
        }
        out.push_str(
            "    ];\n\n    pub const fn method(self) -> &'static str {\n        match self {\n",
        );
        for row in &rows {
            let _ = writeln!(
                out,
                "            Self::{} => {},",
                row.variant.clone().unwrap_or_default(),
                quote(&row.method)
            );
        }
        out.push_str(
            "        }\n    }\n\n    pub const fn tier(self) -> Tier {\n        match self {\n",
        );
        for row in &rows {
            let _ = writeln!(
                out,
                "            Self::{} => Tier::{},",
                row.variant.clone().unwrap_or_default(),
                row.tier
            );
        }
        out.push_str("        }\n    }\n\n    pub const fn params_type(self) -> Option<&'static str> {\n        match self {\n");
        for row in &rows {
            let value = match &row.params {
                Some(params) => format!("Some({})", quote(params)),
                None => "None".into(),
            };
            let _ = writeln!(
                out,
                "            Self::{} => {value},",
                row.variant.clone().unwrap_or_default()
            );
        }
        out.push_str("        }\n    }\n\n");
        if matches!(kind, Kind::ClientRequest | Kind::ServerRequest) {
            out.push_str(
                "    pub const fn response_type(self) -> &'static str {\n        match self {\n",
            );
            for row in &rows {
                let _ = writeln!(
                    out,
                    "            Self::{} => {},",
                    row.variant.clone().unwrap_or_default(),
                    quote(row.response.as_deref().unwrap_or_default())
                );
            }
            out.push_str("        }\n    }\n\n");
        }
        out.push_str(
            "    pub fn from_method(method: &str) -> Option<Self> {\n        match method {\n",
        );
        for row in &rows {
            let _ = writeln!(
                out,
                "            {} => Some(Self::{}),",
                quote(&row.method),
                row.variant.clone().unwrap_or_default()
            );
        }
        out.push_str("            _ => None,\n        }\n    }\n}\n\n");
        let _ = writeln!(
            out,
            "impl std::fmt::Display for {name} {{\n    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        formatter.write_str(self.method())\n    }}\n}}\n"
        );
        let _ = writeln!(
            out,
            "impl serde::Serialize for {name} {{\n    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {{\n        serializer.serialize_str(self.method())\n    }}\n}}\n"
        );
        let _ = writeln!(
            out,
            "impl<'de> serde::Deserialize<'de> for {name} {{\n    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {{\n        let method = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;\n        Self::from_method(&method)\n            .ok_or_else(|| serde::de::Error::custom(format!(\"unknown method: {{method}}\")))\n    }}\n}}\n"
        );
    }
    for kind in Kind::ALL {
        let _ = writeln!(
            out,
            "pub const {}: &[(&str, Disposition)] = &[",
            kind.table_name()
        );
        for row in &contract.rows[&kind] {
            let disposition = match row.tier.as_str() {
                "A" => "Disposition::Selected(Tier::A)",
                "B" => "Disposition::Selected(Tier::B)",
                "C" => "Disposition::C",
                "undecided" => "Disposition::Undecided",
                other => return Err(format!("{}: tier {other}", row.method)),
            };
            let _ = writeln!(out, "    ({}, {disposition}),", quote(&row.method));
        }
        out.push_str("];\n\n");
    }
    out.push_str("fn lookup(table: &[(&str, Disposition)], method: &str) -> Option<Disposition> {\n    table\n        .iter()\n        .find(|(name, _)| *name == method)\n        .map(|(_, disposition)| *disposition)\n}\n\n");
    for (function, kind) in [
        ("client_request_disposition", Kind::ClientRequest),
        ("client_notification_disposition", Kind::ClientNotification),
        ("server_request_disposition", Kind::ServerRequest),
        ("server_notification_disposition", Kind::ServerNotification),
    ] {
        let _ = writeln!(
            out,
            "pub fn {function}(method: &str) -> Option<Disposition> {{\n    lookup({}, method)\n}}\n",
            kind.table_name()
        );
    }
    out.push_str("pub fn unselected_request_error(method: &str) -> Option<i64> {\n    match client_request_disposition(method) {\n        Some(Disposition::Selected(_)) => None,\n        _ => Some(error_code::METHOD_NOT_FOUND),\n    }\n}\n");
    Ok(out)
}

fn cell(value: &str) -> String {
    value.replace('|', "\\|")
}

fn methods_md(contract: &Contract<'_>, items: &[Item]) -> String {
    let manifest = contract.manifest;
    let mut out = String::new();
    let tag = manifest["upstream"]["tag"].as_str().unwrap_or_default();
    let commit = manifest["upstream"]["commit"].as_str().unwrap_or_default();
    let _ = writeln!(out, "# Codex App Server method inventory\n");
    let _ = writeln!(
        out,
        "Generated from `contract/codex-v2.json` (contract `{}` version {}) and the pinned Codex `{tag}` export (commit `{commit}`) by `cargo run -p codewhale-app-server-protocol --bin codewhale-codex-contract -- --write`. Edit the manifest, never this file; `tests/contract.rs` fails when they drift. The method sets are the unmodified export members under `tests/pinned/codex/`.\n",
        manifest["contract"].as_str().unwrap_or_default(),
        manifest["version"]
    );
    out.push_str("- **A**: required for 0.10.3 Codex parity.\n- **B**: service methods, required for 0.10.3.\n- **C**: out of scope, for the reason given.\n- **undecided**: the 0.10.3 parity bar does not settle it yet. The last column carries a recommended tier for the founder to confirm; until then it is handled exactly like Tier C.\n\n");
    out.push_str("Status: `implemented` means a Codex-shaped handler is on main (response goldens are not yet checked); `partial` means the Codex method name dispatches to the legacy shape; `missing` means no handler; `method-not-found` means the dispatcher answers `-32601`; `never-sent` means the server never emits it. The stable export is the scope: methods that exist only in the experimental export are not listed. A complete inventory or typed shape is not a handler: status, not tier, says what main dispatches.\n\n");
    out.push_str("## Dispatch rules\n\n| Tier | Rule |\n| --- | --- |\n");
    if let Some(rules) = manifest["dispatch"].as_object() {
        for (tier, rule) in rules {
            let _ = writeln!(
                out,
                "| {tier} | {} |",
                cell(rule.as_str().unwrap_or_default())
            );
        }
    }
    out.push('\n');
    let mut counts = BTreeMap::new();
    for kind in Kind::ALL {
        let _ = writeln!(out, "## {}\n", kind.section());
        out.push_str("| Method | Tier | Status | Basis | Recommended | Params | Result |\n| --- | --- | --- | --- | --- | --- | --- |\n");
        for row in &contract.rows[&kind] {
            *counts
                .entry((kind.section(), row.tier.clone()))
                .or_insert(0usize) += 1;
            let shown = |value: &Option<String>| match value {
                Some(name) if row.selected() => format!("`{name}`"),
                Some(name) => format!("{name} (untyped)"),
                None => String::new(),
            };
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} |",
                row.method,
                row.tier,
                row.status,
                cell(&row.basis),
                row.recommended.clone().unwrap_or_default(),
                shown(&row.params),
                shown(&row.response),
            );
        }
        out.push('\n');
    }
    out.push_str(
        "## Totals\n\n| Kind | A | B | C | undecided |\n| --- | --- | --- | --- | --- |\n",
    );
    for kind in Kind::ALL {
        let count = |tier: &str| {
            counts
                .get(&(kind.section(), tier.to_owned()))
                .copied()
                .unwrap_or(0)
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            kind.section(),
            count("A"),
            count("B"),
            count("C"),
            count("undecided")
        );
    }
    let _ = writeln!(
        out,
        "\nTyped definitions generated into `src/v2.rs`: {}.\n",
        items.len()
    );
    out.push_str("## Errors\n\nCodex envelopes carry no `jsonrpc` member. ACP keeps its own JSON-RPC 2.0 envelope and is not this contract.\n\n| Name | Code | Meaning |\n| --- | --- | --- |\n");
    if let Some(errors) = manifest["errors"].as_array() {
        for error in errors {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                error["name"].as_str().unwrap_or_default(),
                error["code"],
                cell(error["meaning"].as_str().unwrap_or_default())
            );
        }
    }
    out.push_str("\n## Codewhale extensions\n\nThese ride on Codex shapes without changing any pinned field. The pinned `Thread` schema allows additional properties, so a Codex client ignores the fields. Codewhale notifications live under `codewhale/`, because Codex already uses `thread/environment/*` for its own environments. Extension shapes are declared under `extensions.definitions` in the manifest and generated with the Codex types; none carries a credential.\n\n| Name | Kind | Tier | Status | Shape |\n| --- | --- | --- | --- | --- |\n");
    if let Some(rows) = manifest["extensions"]["rows"].as_array() {
        for row in rows {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} |",
                row["name"].as_str().unwrap_or_default(),
                row["kind"].as_str().unwrap_or_default(),
                row["tier"].as_str().unwrap_or_default(),
                row["status"].as_str().unwrap_or_default(),
                cell(row["shape"].as_str().unwrap_or_default())
            );
        }
    }
    out
}

fn rewrite_refs(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut copy = Map::new();
            for (key, child) in object {
                if key == "$ref"
                    && let Some(reference) = child.as_str()
                    && let Ok(parsed) = Key::parse(reference)
                {
                    copy.insert(
                        key.clone(),
                        Value::String(format!("#/definitions/{}", parsed.name)),
                    );
                } else if key != "$schema" {
                    copy.insert(key.clone(), rewrite_refs(child));
                }
            }
            Value::Object(copy)
        }
        Value::Array(values) => Value::Array(values.iter().map(rewrite_refs).collect()),
        other => other.clone(),
    }
}

fn schema_json(contract: &Contract<'_>, raw: &BTreeMap<String, Value>) -> Result<String, String> {
    let manifest = contract.manifest;
    let mut definitions = Map::new();
    for (name, schema) in raw {
        definitions.insert(name.clone(), rewrite_refs(schema));
    }
    for extension in &contract.extension_fields {
        if let Some(Value::Object(properties)) = definitions
            .get_mut(&extension.owner.name)
            .and_then(|owner| owner.get_mut("properties"))
        {
            properties.insert(extension.field.clone(), rewrite_refs(&extension.schema));
        }
    }
    let mut methods = Map::new();
    for kind in Kind::ALL {
        let mut list = Map::new();
        for row in contract.selected(kind) {
            let mut entry = Map::new();
            entry.insert("tier".into(), Value::String(row.tier.clone()));
            if let Some(params) = &row.params {
                entry.insert(
                    "params".into(),
                    Value::String(format!("#/definitions/{params}")),
                );
            }
            if let Some(response) = &row.response {
                entry.insert(
                    "result".into(),
                    Value::String(format!("#/definitions/{response}")),
                );
            }
            list.insert(row.method.clone(), Value::Object(entry));
        }
        methods.insert(kind.key().into(), Value::Object(list));
    }
    let document = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": manifest["contract"],
        "x-contract-version": manifest["version"],
        "x-upstream": {"tag": manifest["upstream"]["tag"], "commit": manifest["upstream"]["commit"]},
        "x-methods": methods,
        "definitions": definitions,
    });
    serde_json::to_string_pretty(&document)
        .map(|text| text + "\n")
        .map_err(|error| error.to_string())
}

fn ts_ty(ty: &Ty) -> String {
    match ty {
        Ty::String => "string".into(),
        Ty::Bool => "boolean".into(),
        Ty::Int(_) => "number".into(),
        Ty::Json => "unknown".into(),
        Ty::JsonObject => "{ [key: string]: unknown }".into(),
        Ty::Named(name) => name.clone(),
        Ty::Vec(item) => format!("Array<{}>", ts_ty(item)),
        Ty::Map(value) => format!("{{ [key: string]: {} }}", ts_ty(value)),
        Ty::NullableMap(value) => format!("{{ [key: string]: {} | null }}", ts_ty(value)),
    }
}

fn ts_object(fields: &[Field], extra: Option<&str>) -> String {
    let mut members = ts_fields(fields);
    if let Some(extra) = extra {
        if !members.is_empty() {
            members.push_str("; ");
        }
        members.push_str(extra);
    }
    let base = if members.is_empty() {
        "Record<string, never>".to_owned()
    } else {
        format!("{{ {members} }}")
    };
    let flattened: Vec<String> = fields
        .iter()
        .filter(|field| field.flatten)
        .map(|field| ts_ty(&field.ty))
        .collect();
    if flattened.is_empty() {
        base
    } else if members.is_empty() {
        flattened.join(" & ")
    } else {
        format!("{base} & {}", flattened.join(" & "))
    }
}

fn ts_fields(fields: &[Field]) -> String {
    fields
        .iter()
        .filter(|field| !field.flatten)
        .map(|field| {
            let optional = if field.required { "" } else { "?" };
            let nullable = if field.nullable { " | null" } else { "" };
            format!(
                "{}{optional}: {}{nullable}",
                quote(&field.wire),
                ts_ty(&field.ty)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn typescript(contract: &Contract<'_>, items: &[Item]) -> Result<String, String> {
    let mut out = String::new();
    for item in items {
        let name = &item.name;
        let body = match &item.shape {
            Shape::Alias(ty) => ts_ty(ty),
            Shape::Struct {
                fields, additional, ..
            } => ts_object(fields, additional.then_some("[key: string]: unknown")),
            Shape::StringEnum(values) => values
                .iter()
                .map(|value| quote(value))
                .collect::<Vec<_>>()
                .join(" | "),
            Shape::Tagged { tag, variants } => variants
                .iter()
                .map(|(value, fields)| {
                    let tagged = format!("{}: {}", quote(tag), quote(value));
                    ts_object(fields, Some(&tagged))
                })
                .collect::<Vec<_>>()
                .join(" | "),
            Shape::External(variants) => variants
                .iter()
                .map(|variant| match variant {
                    External::Unit(value) => quote(value),
                    External::Newtype(key, ty) => format!("{{ {}: {} }}", quote(key), ts_ty(ty)),
                    External::Struct(key, fields) => {
                        format!("{{ {}: {} }}", quote(key), ts_object(fields, None))
                    }
                })
                .collect::<Vec<_>>()
                .join(" | "),
            Shape::Untagged(variants) => variants
                .iter()
                .map(|(_, ty)| ts_ty(ty))
                .collect::<Vec<_>>()
                .join(" | "),
        };
        let _ = writeln!(out, "export type {name} = {body};");
    }
    for (name, kind, response) in [
        ("ClientRequestMap", Kind::ClientRequest, true),
        ("ClientNotificationMap", Kind::ClientNotification, false),
        ("ServerRequestMap", Kind::ServerRequest, true),
        ("ServerNotificationMap", Kind::ServerNotification, false),
    ] {
        let _ = writeln!(out, "export type {name} = {{");
        for row in contract.selected(kind) {
            let params = row.params.clone().unwrap_or_else(|| "undefined".into());
            if response {
                let _ = writeln!(
                    out,
                    "  {}: {{ params: {params}; result: {} }};",
                    quote(&row.method),
                    row.response.clone().unwrap_or_default()
                );
            } else {
                let _ = writeln!(out, "  {}: {{ params: {params} }};", quote(&row.method));
            }
        }
        out.push_str("};\n");
    }
    Ok(out)
}
