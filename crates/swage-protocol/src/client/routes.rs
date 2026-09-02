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

use super::{
    context::TurnContextResponse,
    conversation::{
        ConversationDetail, ConversationListResponse, ConversationResponse,
        CreateConversationRequest,
    },
    page::{
        CreatePageAnnotationRequest, CreatePageRequest, PageAnnotationView, PageListResponse,
        PageModuleView, PageResponse, UpdatePageAnnotationRequest, WritePageModuleRequest,
    },
    provider_signin::{
        ProviderSignInState, StartProviderSignInRequest, StartProviderSignInResponse,
    },
    settings::{
        ModelDiscoveryResponse, RestartStatusResponse, SaveProviderSettings, SaveSettingsRequest,
        SettingsResponse,
    },
    space::{CreateSpaceRequest, SpaceListResponse, SpaceResponse, UpdateSpaceRequest},
};

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
    /// included.
    pub path: &'static str,
    /// Parameters interpolated into the path template.
    pub path_parameters: Vec<Parameter>,
    /// Optional values rendered after the path as a query string.
    pub query_parameters: Vec<QueryParameter>,
    /// A one-line description for the document and the client's doc
    /// comment.
    pub summary: &'static str,
    /// The request body's schema name, when the route takes one.
    pub request: Option<Cow<'static, str>>,
    /// Whether callers may omit the request body altogether.
    pub request_optional: bool,
    /// The success status and the response body's schema name; `None` is
    /// a bodiless success.
    pub response: Response,
}

/// The wire representation used for a path or query value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterType {
    Uuid,
    String,
}

impl ParameterType {
    pub const fn openapi_format(self) -> Option<&'static str> {
        match self {
            Self::Uuid => Some("uuid"),
            Self::String => None,
        }
    }

    pub const fn probe_value(self) -> &'static str {
        match self {
            Self::Uuid => "not-a-uuid",
            Self::String => "probe",
        }
    }
}

/// One value interpolated into a route path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: &'static str,
    pub kind: ParameterType,
}

/// One value appended to a route query string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryParameter {
    pub name: &'static str,
    pub kind: ParameterType,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Option<Cow<'static, str>>,
}

impl Route {
    /// Render this template with deterministic values suitable for a router probe.
    pub fn probe_path(&self) -> String {
        self.path_parameters
            .iter()
            .fold(self.path.to_owned(), |path, parameter| {
                path.replace(
                    &format!("{{{}}}", parameter.name),
                    parameter.kind.probe_value(),
                )
            })
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
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "List every space.",
            request_optional: false,

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
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Create a space.",
            request_optional: false,

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
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read one space.",
            request_optional: false,

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
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Update a space's name or description.",
            request_optional: false,

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
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Delete a space.",
            request_optional: false,

            request: None,
            response: Response {
                status: 204,
                body: None,
            },
        },
        Route {
            operation_id: "list_space_sessions",
            method: Method::Get,
            path: "/api/spaces/{space_id}/sessions",
            path_parameters: vec![Parameter {
                name: "space_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "List conversations in a space.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationListResponse>(),
            },
        },
        Route {
            operation_id: "create_space_session",
            method: Method::Post,
            path: "/api/spaces/{space_id}/sessions",
            path_parameters: vec![Parameter {
                name: "space_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Create a conversation in a space.",
            request_optional: true,

            request: schema::<CreateConversationRequest>(),
            response: Response {
                status: 201,
                body: schema::<ConversationResponse>(),
            },
        },
        Route {
            operation_id: "get_conversation",
            method: Method::Get,
            path: "/api/conversations/{id}",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read a conversation.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationDetail>(),
            },
        },
        Route {
            operation_id: "delete_conversation",
            method: Method::Delete,
            path: "/api/conversations/{id}",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Delete a conversation.",
            request_optional: false,

            request: None,
            response: Response {
                status: 204,
                body: None,
            },
        },
        Route {
            operation_id: "get_turn_context",
            method: Method::Get,
            path: "/api/turns/{id}/context",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read a turn context report.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<TurnContextResponse>(),
            },
        },
        Route {
            operation_id: "list_pages",
            method: Method::Get,
            path: "/api/spaces/{space_id}/pages",
            path_parameters: vec![Parameter {
                name: "space_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "List pages in a space.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<PageListResponse>(),
            },
        },
        Route {
            operation_id: "create_page",
            method: Method::Post,
            path: "/api/spaces/{space_id}/pages",
            path_parameters: vec![Parameter {
                name: "space_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Create a page in a space.",
            request_optional: false,

            request: schema::<CreatePageRequest>(),
            response: Response {
                status: 200,
                body: schema::<PageResponse>(),
            },
        },
        Route {
            operation_id: "get_page",
            method: Method::Get,
            path: "/api/pages/{page_id}",
            path_parameters: vec![Parameter {
                name: "page_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read a page.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<PageResponse>(),
            },
        },
        Route {
            operation_id: "write_page_module",
            method: Method::Post,
            path: "/api/pages/{page_id}/modules",
            path_parameters: vec![Parameter {
                name: "page_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Write a page module.",
            request_optional: false,

            request: schema::<WritePageModuleRequest>(),
            response: Response {
                status: 200,
                body: schema::<PageModuleView>(),
            },
        },
        Route {
            operation_id: "create_page_annotation",
            method: Method::Post,
            path: "/api/pages/{page_id}/annotations",
            path_parameters: vec![Parameter {
                name: "page_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Create a page annotation.",
            request_optional: false,

            request: schema::<CreatePageAnnotationRequest>(),
            response: Response {
                status: 200,
                body: schema::<PageAnnotationView>(),
            },
        },
        Route {
            operation_id: "update_page_annotation",
            method: Method::Post,
            path: "/api/page-annotations/{annotation_id}",
            path_parameters: vec![Parameter {
                name: "annotation_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Update a page annotation.",
            request_optional: false,

            request: schema::<UpdatePageAnnotationRequest>(),
            response: Response {
                status: 200,
                body: schema::<PageAnnotationView>(),
            },
        },
        Route {
            operation_id: "delete_page_annotation",
            method: Method::Delete,
            path: "/api/page-annotations/{annotation_id}",
            path_parameters: vec![Parameter {
                name: "annotation_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Delete a page annotation.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: None,
            },
        },
        Route {
            operation_id: "get_settings",
            method: Method::Get,
            path: "/api/settings",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Read gateway settings.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<SettingsResponse>(),
            },
        },
        Route {
            operation_id: "save_settings",
            method: Method::Put,
            path: "/api/settings",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Save gateway settings.",
            request_optional: false,

            request: schema::<SaveSettingsRequest>(),
            response: Response {
                status: 200,
                body: schema::<SettingsResponse>(),
            },
        },
        Route {
            operation_id: "discover_provider_models",
            method: Method::Get,
            path: "/api/settings/providers/{id}/models",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::String,
            }],
            query_parameters: vec![QueryParameter {
                name: "q",
                kind: ParameterType::String,
                required: false,
            }],
            summary: "Discover models for a configured provider.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ModelDiscoveryResponse>(),
            },
        },
        Route {
            operation_id: "discover_draft_provider_models",
            method: Method::Post,
            path: "/api/settings/providers/{id}/models",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::String,
            }],
            query_parameters: vec![QueryParameter {
                name: "q",
                kind: ParameterType::String,
                required: false,
            }],
            summary: "Discover models for draft provider settings.",
            request_optional: false,

            request: schema::<SaveProviderSettings>(),
            response: Response {
                status: 200,
                body: schema::<ModelDiscoveryResponse>(),
            },
        },
        Route {
            operation_id: "get_provider_sign_in_state",
            method: Method::Get,
            path: "/api/settings/providers/{id}/sign-in",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::String,
            }],
            query_parameters: vec![],
            summary: "Read provider sign-in state.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ProviderSignInState>(),
            },
        },
        Route {
            operation_id: "start_provider_sign_in",
            method: Method::Post,
            path: "/api/settings/providers/{id}/sign-in",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::String,
            }],
            query_parameters: vec![],
            summary: "Start provider sign-in.",
            request_optional: false,

            request: schema::<StartProviderSignInRequest>(),
            response: Response {
                status: 200,
                body: schema::<StartProviderSignInResponse>(),
            },
        },
        Route {
            operation_id: "sign_out_provider",
            method: Method::Delete,
            path: "/api/settings/providers/{id}/sign-in",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::String,
            }],
            query_parameters: vec![],
            summary: "Sign out a provider.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ProviderSignInState>(),
            },
        },
        Route {
            operation_id: "get_restart_status",
            method: Method::Get,
            path: "/api/settings/restart-required",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Read restart status.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<RestartStatusResponse>(),
            },
        },
        Route {
            operation_id: "restart_gateway",
            method: Method::Post,
            path: "/api/settings/restart",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Restart the gateway.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<RestartStatusResponse>(),
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_unique_and_path_parameters_match_the_template() {
        let routes = routes();
        let mut ids: Vec<&str> = routes.iter().map(|route| route.operation_id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), routes.len(), "operation ids must be unique");
        for route in &routes {
            for parameter in &route.path_parameters {
                assert!(
                    !parameter.name.is_empty()
                        && parameter
                            .name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_'),
                    "{}: parameter {parameter:?} is not a plain name",
                    route.path,
                );
            }
            let template_parameters: Vec<_> = route
                .path
                .split('/')
                .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
                .collect();
            assert_eq!(
                route
                    .path_parameters
                    .iter()
                    .map(|parameter| parameter.name)
                    .collect::<Vec<_>>(),
                template_parameters,
                "{}: path metadata must match its template",
                route.path,
            );
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
    fn path_parameters_render_their_declared_probe_values() {
        let get = routes()
            .into_iter()
            .find(|route| route.operation_id == "get_space")
            .unwrap();
        assert_eq!(get.probe_path(), "/api/spaces/not-a-uuid");
    }

    #[test]
    fn non_uuid_path_parameters_use_their_declared_probe_value() {
        let discover = routes()
            .into_iter()
            .find(|route| route.operation_id == "discover_provider_models")
            .unwrap();
        assert_eq!(
            discover.probe_path(),
            "/api/settings/providers/probe/models"
        );
    }
}
