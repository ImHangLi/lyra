//! JSON Schemas generated from the Rust wire types, and the protocol hash used in `hello`.

use std::sync::OnceLock;

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, SchemaGenerator};
use serde_json::{Value, json};

use crate::ids::Digest;

/// Public schema names accepted by `mira schema NAME`.
pub const NAMES: &[&str] = &[
    "workspace",
    "plugin",
    "local",
    "invocation",
    "plugin-event",
    "cli-reply",
    "ipc",
];

fn root<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>();
    serde_json::to_value(schema).unwrap_or(Value::Null)
}

fn ipc_schema() -> Value {
    let mut generator: SchemaGenerator = SchemaSettings::draft2020_12().into_generator();
    let methods = crate::ipc::method_schemas(&mut generator);
    let request = generator.subschema_for::<crate::ipc::RpcRequest>();
    let response = generator.subschema_for::<crate::ipc::RpcResponse>();
    let notification = generator.subschema_for::<crate::ipc::RpcNotification>();
    let reply = generator.subschema_for::<crate::reply::PublicReply<Value>>();
    let defs = generator.take_definitions(true);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "MIPC/1",
        "description": "JSON-RPC 2.0 single-object profile, LF framed. Non-hello results are PublicReply objects whose data matches the method result.",
        "oneOf": [request, response, notification],
        "x-mira-methods": methods,
        "x-mira-public-reply": reply,
        "$defs": defs,
    })
}

/// The schema for `name`, or `None` for an unknown name.
pub fn schema(name: &str) -> Option<Value> {
    Some(match name {
        "workspace" => root::<crate::manifest::WorkspaceWire>(),
        "plugin" => root::<crate::manifest::PluginWire>(),
        "local" => root::<crate::manifest::LocalWire>(),
        "invocation" => root::<crate::mpp::Invocation>(),
        "plugin-event" => root::<crate::mpp::PluginFrame>(),
        "cli-reply" => root::<crate::reply::PublicReply<Value>>(),
        "ipc" => ipc_schema(),
        _ => return None,
    })
}

/// SHA-256 over the JCS form of every public schema. Clients and hosts built from
/// different contracts refuse each other in `hello`.
pub fn protocol_hash() -> &'static Digest {
    static HASH: OnceLock<Digest> = OnceLock::new();
    HASH.get_or_init(|| {
        let all: Vec<(String, Value)> = NAMES
            .iter()
            .map(|n| ((*n).to_owned(), schema(n).unwrap_or(Value::Null)))
            .collect();
        crate::hash::canonical_digest(&all)
            .unwrap_or_else(|_| Digest::of_bytes(b"unhashable-schema-set"))
    })
}
