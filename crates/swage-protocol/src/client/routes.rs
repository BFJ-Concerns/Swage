//! The client surface's HTTP route manifest: every route named once, with
//! its method, path template, path parameters, request body, and response
//! body, so the OpenAPI document and the generated TypeScript client are
//! projections of the same table the router is checked against.
//!
//! A route's request and response types are the protocol crate's own; the
//! manifest names them by their schema name, which is how the JSON Schema
//! document already refers to them under `$defs`. Failures are not listed
//! per route: every route answers a failure with
//! [`super::error::ApiErrorBody`], projected once as the shared response.

use std::borrow::Cow;

use schemars::JsonSchema;

use super::space::{CreateSpaceRequest, SpaceListResponse, SpaceResponse, UpdateSpaceRequest};

/// HTTP method, as the OpenAPI document spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "get",
            Method::Post => "post",
            Method::Put => "put",
            Method::Delete => "delete",
        }
    }
}

/// One route on the client surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The operation's identity — the generated client's function name.
    pub operation_id: &'static str,
    pub method: Method,
    /// The path template as the router registers it, `{name}` parameters
    /// included; every parameter is a UUID string.
    pub path: &'static str,
    /// A one-line description for the document and the client's doc
    /// comment.
    pub summary: &'static str,
    /// The request body's schema name, when the route takes one.
    pub request: Option<Cow<'static, str>>,
    /// The success status and the response body's schema name; `None` is
    /// a bodiless success.
    pub response: Response,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Option<Cow<'static, str>>,
}

impl Route {
    /// The `{name}` parameters in the path template, in order.
    pub fn path_parameters(&self) -> Vec<&'static str> {
        self.path
            .split('/')
            .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
            .collect()
    }
}

fn schema<T: JsonSchema>() -> Option<Cow<'static, str>> {
    Some(T::schema_name())
}

/// Every route on the client surface, in registration order.
///
/// Adding a route is one entry here beside its handler: the OpenAPI
/// document, the generated client, and the router-coverage test all read
/// this table.
pub fn routes() -> Vec<Route> {
    vec![
        Route {
            operation_id: "list_spaces",
            method: Method::Get,
            path: "/api/spaces",
            summary: "List every space.",
            request: None,
            response: Response {
                status: 200,
                body: schema::<SpaceListResponse>(),
            },
        },
        Route {
            operation_id: "create_space",
            method: Method::Post,
            path: "/api/spaces",
            summary: "Create a space.",
            request: schema::<CreateSpaceRequest>(),
            response: Response {
                status: 201,
                body: schema::<SpaceResponse>(),
            },
        },
        Route {
            operation_id: "get_space",
            method: Method::Get,
            path: "/api/spaces/{id}",
            summary: "Read one space.",
            request: None,
            response: Response {
                status: 200,
                body: schema::<SpaceResponse>(),
            },
        },
        Route {
            operation_id: "update_space",
            method: Method::Put,
            path: "/api/spaces/{id}",
            summary: "Update a space's name or description.",
            request: schema::<UpdateSpaceRequest>(),
            response: Response {
                status: 200,
                body: schema::<SpaceResponse>(),
            },
        },
        Route {
            operation_id: "delete_space",
            method: Method::Delete,
            path: "/api/spaces/{id}",
            summary: "Delete a space.",
            request: None,
            response: Response {
                status: 204,
                body: None,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_unique_and_paths_carry_only_uuid_parameters() {
        let routes = routes();
        let mut ids: Vec<&str> = routes.iter().map(|route| route.operation_id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), routes.len(), "operation ids must be unique");
        for route in &routes {
            for parameter in route.path_parameters() {
                assert!(
                    !parameter.is_empty() && parameter.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                    "{}: parameter {parameter:?} is not a plain name",
                    route.path
                );
            }
        }
    }

    #[test]
    fn every_named_schema_is_a_client_surface_type() {
        // The document refers to bodies under `$defs`; a name outside the
        // surface's schema would be a dangling reference.
        let schema = schemars::schema_for!(super::super::ClientSurface);
        let defs = schema
            .as_value()
            .get("$defs")
            .and_then(|defs| defs.as_object())
            .expect("client schema has $defs");
        for route in routes() {
            for name in route.request.iter().chain(route.response.body.iter()) {
                assert!(
                    defs.contains_key(name.as_ref()),
                    "{}: {name} is not on the client surface",
                    route.operation_id
                );
            }
        }
    }

    #[test]
    fn path_parameters_are_read_from_the_template() {
        let get = routes()
            .into_iter()
            .find(|route| route.operation_id == "get_space")
            .unwrap();
        assert_eq!(get.path_parameters(), vec!["id"]);
    }
}
