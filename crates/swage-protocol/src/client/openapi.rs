//! The OpenAPI 3.1 projection of the client surface: the route manifest
//! rendered as paths, with the surface's JSON Schema as its components, so
//! the document and the schema never disagree.

use serde_json::{Map, Value, json};

use super::error::ApiErrorBody;
use super::routes::{Route, routes};

/// Render the OpenAPI document for the client surface. The output is
/// deterministic: same routes and schema, same bytes.
pub fn document() -> Value {
    let surface = schemars::schema_for!(super::ClientSurface).to_value();
    let mut defs = surface
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // A root schema refers to its definitions as `#/$defs/Name`; OpenAPI
    // keeps them under components, so every reference is rewritten.
    for value in defs.values_mut() {
        rewrite_refs(value);
    }
    let mut paths = Map::new();
    for route in routes() {
        let entry = paths
            .entry(route.path.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        entry[route.method.as_str()] = operation(&route);
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Swage client surface",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "paths": paths,
        "components": {
            "schemas": defs,
            "responses": {
                "Error": {
                    "description": "The request failed; every failure carries this body.",
                    "content": {"application/json": {"schema": component_ref(&<ApiErrorBody as schemars::JsonSchema>::schema_name())}},
                }
            }
        }
    })
}

fn operation(route: &Route) -> Value {
    let parameters: Vec<Value> = route
        .path_parameters()
        .into_iter()
        .map(|name| {
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "schema": {"type": "string", "format": "uuid"},
            })
        })
        .collect();
    let mut responses = Map::new();
    responses.insert(
        route.response.status.to_string(),
        match route.response.body.as_ref() {
            Some(body) => json!({
                "description": "Success.",
                "content": {"application/json": {"schema": component_ref(body)}},
            }),
            None => json!({"description": "Success, no body."}),
        },
    );
    responses.insert(
        "default".to_string(),
        json!({"$ref": "#/components/responses/Error"}),
    );
    let mut operation = json!({
        "operationId": route.operation_id,
        "summary": route.summary,
        "parameters": parameters,
        "responses": responses,
    });
    if let Some(request) = route.request.as_ref() {
        operation["requestBody"] = json!({
            "required": true,
            "content": {"application/json": {"schema": component_ref(request)}},
        });
    }
    operation
}

fn component_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get_mut("$ref")
                && let Some(name) = reference.strip_prefix("#/$defs/")
            {
                *reference = format!("#/components/schemas/{name}");
            }
            for child in object.values_mut() {
                rewrite_refs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_document_names_every_route_under_its_path_and_method() {
        let document = document();
        for route in routes() {
            let operation = &document["paths"][route.path][route.method.as_str()];
            assert_eq!(operation["operationId"], route.operation_id, "{route:?}");
        }
    }

    #[test]
    fn bodies_and_references_resolve_inside_components() {
        let document = document();
        let schemas = document["components"]["schemas"]
            .as_object()
            .expect("component schemas");
        assert!(schemas.contains_key("SpaceResponse"));
        assert!(schemas.contains_key("ApiErrorBody"));
        let create = &document["paths"]["/api/spaces"]["post"];
        assert_eq!(
            create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CreateSpaceRequest"
        );
        assert_eq!(
            create["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SpaceResponse"
        );
        assert_eq!(
            create["responses"]["default"]["$ref"],
            "#/components/responses/Error"
        );
        let rendered = serde_json::to_string(&document).unwrap();
        assert!(
            !rendered.contains("#/$defs/"),
            "every schema reference is rewritten to components"
        );
    }

    #[test]
    fn a_bodiless_success_and_path_parameters_render() {
        let document = document();
        let delete = &document["paths"]["/api/spaces/{id}"]["delete"];
        assert_eq!(delete["responses"]["204"]["description"], "Success, no body.");
        assert_eq!(delete["parameters"][0]["name"], "id");
        assert_eq!(delete["parameters"][0]["schema"]["format"], "uuid");
    }

    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(
            serde_json::to_string(&document()).unwrap(),
            serde_json::to_string(&document()).unwrap()
        );
    }
}
