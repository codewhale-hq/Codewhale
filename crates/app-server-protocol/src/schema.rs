use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SchemaMode {
    Host,
    Runtime,
}

/// A field's wire type, normalized.
pub enum Ty {
    Nullable(Box<Ty>),
    String,
    Boolean,
    Uint,
    Integer,
    /// A JSON object with any keys (`Map<String, Value>`).
    Object,
    /// Any JSON value.
    Json,
    Ref(String),
    /// A string tag inside a union member.
    Const(String),
    Array(Box<Ty>),
}

pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub required: bool,
}

pub enum Def {
    Object {
        strict: bool,
        extra: bool,
        fields: Vec<Field>,
    },
    Enum(Vec<String>),
    Union(Vec<Vec<Field>>),
}

pub fn ref_name(schema: &Value) -> Option<String> {
    let reference = schema.get("$ref")?.as_str()?;
    Some(
        reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("unexpected $ref `{reference}`"))
            .to_string(),
    )
}

fn parse_ty(at: &str, schema: &Value, mode: SchemaMode) -> Ty {
    let Some(object) = schema.as_object() else {
        assert_eq!(schema, &Value::Bool(true), "{at}: unsupported schema");
        return Ty::Json;
    };
    if let Some(name) = ref_name(schema) {
        return Ty::Ref(name);
    }
    if let Some(variants) = object.get("anyOf").and_then(Value::as_array)
        && variants.len() == 2
        && variants
            .iter()
            .filter(|variant| variant.get("type").and_then(Value::as_str) == Some("null"))
            .count()
            == 1
    {
        let present = parse_ty(
            at,
            variants
                .iter()
                .find(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
                .expect("one non-null variant"),
            mode,
        );
        return if mode == SchemaMode::Runtime {
            Ty::Nullable(Box::new(present))
        } else {
            present
        };
    }
    if let Some(value) = object.get("const").and_then(Value::as_str) {
        return Ty::Const(value.to_string());
    }
    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(ty)) => vec![ty.as_str()],
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .filter(|ty| *ty != "null")
            .collect(),
        // Only annotations (`default`, `description`): any value.
        None if !["enum", "anyOf", "oneOf", "allOf", "properties", "items"]
            .iter()
            .any(|key| object.contains_key(*key)) =>
        {
            return Ty::Json;
        }
        _ => panic!("{at}: unsupported wire schema {schema}"),
    };
    let value = match types.as_slice() {
        ["string"] => Ty::String,
        ["boolean"] => Ty::Boolean,
        ["integer"] if object.get("minimum").and_then(Value::as_u64) == Some(0) => Ty::Uint,
        ["integer"] => Ty::Integer,
        ["object"]
            if object.get("additionalProperties") == Some(&Value::Bool(true))
                && !object.contains_key("properties") =>
        {
            Ty::Object
        }
        ["array"] => Ty::Array(Box::new(parse_ty(
            &format!("{at}[]"),
            object
                .get("items")
                .unwrap_or_else(|| panic!("{at}: array without items")),
            mode,
        ))),
        _ => panic!("{at}: unsupported wire schema {schema}"),
    };
    if mode == SchemaMode::Runtime
        && object
            .get("type")
            .and_then(Value::as_array)
            .is_some_and(|types| types.iter().any(|ty| ty == "null"))
    {
        Ty::Nullable(Box::new(value))
    } else {
        value
    }
}

fn object_fields(at: &str, schema: &Value, mode: SchemaMode) -> Vec<Field> {
    assert_eq!(
        schema.get("type").and_then(Value::as_str),
        Some("object"),
        "{at}: expected an object schema, got {schema}"
    );
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, schema)| Field {
                    name: name.clone(),
                    ty: parse_ty(&format!("{at}.{name}"), schema, mode),
                    required: required.contains(&name.as_str()),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_def(name: &str, schema: &Value, mode: SchemaMode) -> Def {
    let members = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array);
    if let Some(members) = members {
        // Unit variants, with or without doc comments: a string enum.
        let consts: Option<Vec<String>> = members
            .iter()
            .map(|member| {
                member
                    .get("const")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        return match consts {
            Some(values) => Def::Enum(values),
            None if members.len() == 1 => Def::Object {
                strict: members[0].get("additionalProperties") == Some(&Value::Bool(false)),
                extra: mode == SchemaMode::Runtime
                    && members[0].get("additionalProperties") != Some(&Value::Bool(false)),
                fields: object_fields(name, &members[0], mode),
            },
            None => Def::Union(
                members
                    .iter()
                    .map(|member| object_fields(name, member, mode))
                    .collect(),
            ),
        };
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return Def::Enum(
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("{name}: non-string enum value"))
                        .to_string()
                })
                .collect(),
        );
    }
    Def::Object {
        strict: schema.get("additionalProperties") == Some(&Value::Bool(false)),
        extra: mode == SchemaMode::Runtime
            && schema.get("additionalProperties") != Some(&Value::Bool(false)),
        fields: object_fields(name, schema, mode),
    }
}

pub fn quote(value: &str) -> String {
    assert!(
        !value.contains(['\'', '\\']),
        "`{value}` needs escaping in TypeScript"
    );
    format!("'{value}'")
}

fn ts(ty: &Ty) -> String {
    match ty {
        Ty::Nullable(inner) => format!("({} | null)", ts(inner)),
        Ty::String => "string".into(),
        Ty::Boolean => "boolean".into(),
        Ty::Uint | Ty::Integer => "number".into(),
        Ty::Object => "{ [key: string]: Json }".into(),
        Ty::Json => "Json".into(),
        Ty::Ref(name) => name.clone(),
        Ty::Const(value) => quote(value),
        Ty::Array(item) => format!("{}[]", ts(item)),
    }
}

/// An array's item type, however deeply nested.
pub fn innermost(ty: &Ty) -> &Ty {
    match ty {
        Ty::Array(item) => innermost(item),
        ty => ty,
    }
}

fn member(field: &Field) -> String {
    let optional = if field.required { "" } else { "?" };
    format!("{}{optional}: {}", field.name, ts(&field.ty))
}

pub fn typescript_defs(defs: &BTreeMap<String, Def>, mode: SchemaMode) -> String {
    let mut out = String::from(if mode == SchemaMode::Host {
        "\nexport type Json = null | boolean | number | string | Json[] | { [key: string]: Json }\n"
    } else {
        "\nexport type Json = null | boolean | number | string | Json[] | { [key: string]: Json | undefined }\n"
    });
    for (name, def) in defs {
        out.push('\n');
        match def {
            Def::Object { fields, extra, .. } if fields.is_empty() && !extra => {
                if mode == SchemaMode::Host {
                    let _ = writeln!(out, "export interface {name} {{}}");
                } else {
                    let _ = writeln!(out, "export type {name} = Record<string, never>");
                }
            }
            Def::Object { fields, extra, .. } => {
                let declaration = if mode == SchemaMode::Host {
                    format!("export interface {name}")
                } else {
                    format!("export type {name} =")
                };
                let _ = writeln!(out, "{declaration} {{");
                for field in fields {
                    let _ = writeln!(out, "  {}", member(field));
                }
                if *extra {
                    out.push_str("  [key: string]: Json | undefined\n");
                }
                out.push_str("}\n");
            }
            Def::Enum(values) => {
                let values: Vec<String> = values.iter().map(|v| quote(v)).collect();
                let _ = writeln!(out, "export type {name} = {}", values.join(" | "));
            }
            Def::Union(members) => {
                let members: Vec<String> = members
                    .iter()
                    .map(|fields| {
                        // Tags first, then the variant's fields in order.
                        let (tags, rest): (Vec<&Field>, Vec<&Field>) = fields
                            .iter()
                            .partition(|field| matches!(field.ty, Ty::Const(_)));
                        let parts: Vec<String> = tags.into_iter().chain(rest).map(member).collect();
                        format!("{{ {} }}", parts.join("; "))
                    })
                    .collect();
                let _ = writeln!(out, "export type {name} = {}", members.join(" | "));
            }
        }
    }
    out
}
