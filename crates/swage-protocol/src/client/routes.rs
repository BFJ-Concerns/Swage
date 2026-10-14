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
    access::{AccessStatusResponse, PairClientRequest, PairClientResponse, PairingCodeResponse},
    context::TurnContextResponse,
    conversation::{
        ConversationBranchListResponse, ConversationBranchResponse, ConversationDetail,
        ConversationListResponse, ConversationResponse, ConversationThreadDetail,
        CreateConversationBranchRequest, CreateConversationRequest, RecentSessionsResponse,
    },
    docs::{DocumentListResponse, DocumentResponse},
    memory::{
        MemoryInstructionRequest, MemoryInstructionResponse, MemoryPageListResponse,
        MemoryPageResponse,
    },
    provider_signin::{
        ProviderSignInState, StartProviderSignInRequest, StartProviderSignInResponse,
    },
    security_settings::{SaveSecuritySettingsRequest, SecuritySettingsResponse},
    settings::{
        ModelDiscoveryResponse, RestartStatusResponse, SaveProviderSettings, SaveSettingsRequest,
        SettingsResponse,
    },
    surface::{
        CreateSurfaceAnnotationRequest, CreateSurfaceRequest, DeleteSurfaceAnnotationResponse,
        DeleteSurfaceModuleResponse, LibraryEntryResponse, LibraryListResponse,
        MoveSurfaceModuleRequest, SaveSurfaceToLibraryRequest, SurfaceAnnotationView,
        SurfaceListResponse, SurfaceResponse, TopicDefaultSurfaceRequest,
        UpdateSurfaceAnnotationRequest, WriteSurfaceModuleRequest, WriteSurfaceModuleResponse,
    },
    topic::{
        CreateTopicRequest, RelinkWorkspaceAttachmentRequest, TopicListResponse, TopicResponse,
        UpdateTopicRequest, WorkspaceAttachmentResponse,
    },
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
    Boolean,
}

impl ParameterType {
    pub const fn openapi_type(self) -> &'static str {
        match self {
            Self::Uuid | Self::String => "string",
            Self::Boolean => "boolean",
        }
    }

    pub const fn openapi_format(self) -> Option<&'static str> {
        match self {
            Self::Uuid => Some("uuid"),
            Self::String | Self::Boolean => None,
        }
    }

    pub const fn probe_value(self) -> &'static str {
        match self {
            Self::Uuid => "not-a-uuid",
            Self::String => "probe",
            Self::Boolean => "true",
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
            operation_id: "list_topics",
            method: Method::Get,
            path: "/api/topics",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "List every topic.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<TopicListResponse>(),
            },
        },
        Route {
            operation_id: "create_topic",
            method: Method::Post,
            path: "/api/topics",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Create a topic.",
            request_optional: false,

            request: schema::<CreateTopicRequest>(),
            response: Response {
                status: 201,
                body: schema::<TopicResponse>(),
            },
        },
        Route {
            operation_id: "get_topic",
            method: Method::Get,
            path: "/api/topics/{id}",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read one topic.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<TopicResponse>(),
            },
        },
        Route {
            operation_id: "update_topic",
            method: Method::Put,
            path: "/api/topics/{id}",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Update a topic's name or description.",
            request_optional: false,

            request: schema::<UpdateTopicRequest>(),
            response: Response {
                status: 200,
                body: schema::<TopicResponse>(),
            },
        },
        Route {
            operation_id: "delete_topic",
            method: Method::Delete,
            path: "/api/topics/{id}",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Delete a topic.",
            request_optional: false,

            request: None,
            response: Response {
                status: 204,
                body: None,
            },
        },
        Route {
            operation_id: "relink_topic_workspace",
            method: Method::Put,
            path: "/api/topics/{topic_id}/workspaces/{workspace_id}/relink",
            path_parameters: vec![
                Parameter {
                    name: "topic_id",
                    kind: ParameterType::Uuid,
                },
                Parameter {
                    name: "workspace_id",
                    kind: ParameterType::Uuid,
                },
            ],
            query_parameters: vec![],
            summary: "Relink a topic workspace attachment to a moved folder.",
            request: schema::<RelinkWorkspaceAttachmentRequest>(),
            request_optional: false,
            response: Response {
                status: 200,
                body: schema::<WorkspaceAttachmentResponse>(),
            },
        },
        Route {
            operation_id: "list_conversations",
            method: Method::Get,
            path: "/api/conversations",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "List the conversations that run in the general scope — started without a topic.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationListResponse>(),
            },
        },
        Route {
            operation_id: "create_conversation",
            method: Method::Post,
            path: "/api/conversations",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Create a conversation without a topic: a general assistant in the general scope.",
            request_optional: true,

            request: schema::<CreateConversationRequest>(),
            response: Response {
                status: 201,
                body: schema::<ConversationResponse>(),
            },
        },
        Route {
            operation_id: "list_recent_sessions",
            method: Method::Get,
            path: "/api/sessions",
            path_parameters: vec![],
            query_parameters: vec![
                QueryParameter {
                    name: "topic_id",
                    kind: ParameterType::Uuid,
                    required: false,
                },
                QueryParameter {
                    name: "archived",
                    kind: ParameterType::Boolean,
                    required: false,
                },
            ],
            summary: "The recent-sessions list across every scope, newest activity first, automated sessions grouped beneath their source; `topic_id` narrows to a topic and `archived=true` lists the archived sessions instead.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<RecentSessionsResponse>(),
            },
        },
        Route {
            operation_id: "list_topic_sessions",
            method: Method::Get,
            path: "/api/topics/{topic_id}/sessions",
            path_parameters: vec![Parameter {
                name: "topic_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "List conversations in a topic.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationListResponse>(),
            },
        },
        Route {
            operation_id: "create_topic_session",
            method: Method::Post,
            path: "/api/topics/{topic_id}/sessions",
            path_parameters: vec![Parameter {
                name: "topic_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Create a conversation in a topic.",
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
            operation_id: "list_conversation_branches",
            method: Method::Get,
            path: "/api/conversations/{id}/branches",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "List a conversation's branches: the sibling threads taken from earlier messages.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationBranchListResponse>(),
            },
        },
        Route {
            operation_id: "create_conversation_branch",
            method: Method::Post,
            path: "/api/conversations/{id}/branches",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Branch a conversation at an earlier message into a sibling thread.",
            request_optional: false,

            request: schema::<CreateConversationBranchRequest>(),
            response: Response {
                status: 201,
                body: schema::<ConversationBranchResponse>(),
            },
        },
        Route {
            operation_id: "get_thread",
            method: Method::Get,
            path: "/api/threads/{id}",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read one thread with its messages and, for a branch, its lineage.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationThreadDetail>(),
            },
        },
        Route {
            operation_id: "abandon_thread",
            method: Method::Post,
            path: "/api/threads/{id}/abandon",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Abandon a branch: it keeps its history and lineage but takes no further turns.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<ConversationThreadDetail>(),
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
            operation_id: "list_memory_pages",
            method: Method::Get,
            path: "/api/memory/pages",
            path_parameters: vec![],
            query_parameters: vec![QueryParameter {
                name: "topic_id",
                kind: ParameterType::Uuid,
                required: false,
            }],
            summary: "List every memory page of one scope — general memory, or a topic's.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<MemoryPageListResponse>(),
            },
        },
        Route {
            operation_id: "get_memory_page",
            method: Method::Get,
            path: "/api/memory/page",
            path_parameters: vec![],
            query_parameters: vec![
                QueryParameter {
                    name: "reference",
                    kind: ParameterType::String,
                    required: true,
                },
                QueryParameter {
                    name: "topic_id",
                    kind: ParameterType::Uuid,
                    required: false,
                },
            ],
            summary: "Read one memory page, whole, by the reference the index spells.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<MemoryPageResponse>(),
            },
        },
        Route {
            operation_id: "apply_memory_instruction",
            method: Method::Post,
            path: "/api/memory/instructions",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Change memory by instructing the platform's curator; the reply shows every page it changed.",
            request_optional: false,

            request: schema::<MemoryInstructionRequest>(),
            response: Response {
                status: 200,
                body: schema::<MemoryInstructionResponse>(),
            },
        },
        Route {
            operation_id: "list_surfaces",
            method: Method::Get,
            path: "/api/sessions/{session_id}/surfaces",
            path_parameters: vec![Parameter {
                name: "session_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "List a session's surfaces — the work surfaces open with it.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<SurfaceListResponse>(),
            },
        },
        Route {
            operation_id: "create_surface",
            method: Method::Post,
            path: "/api/sessions/{session_id}/surfaces",
            path_parameters: vec![Parameter {
                name: "session_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Create a surface for a session: blank, or seeded from a library entry.",
            request_optional: false,

            request: schema::<CreateSurfaceRequest>(),
            response: Response {
                status: 200,
                body: schema::<SurfaceResponse>(),
            },
        },
        Route {
            operation_id: "list_library",
            method: Method::Get,
            path: "/api/surface-library",
            path_parameters: vec![],
            query_parameters: vec![QueryParameter {
                name: "tag",
                kind: ParameterType::String,
                required: false,
            }],
            summary: "The shared surface library's catalogue: names, descriptions and tags, never module content. `tag` narrows to entries carrying it.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<LibraryListResponse>(),
            },
        },
        Route {
            operation_id: "get_library_entry",
            method: Method::Get,
            path: "/api/surface-library/{entry_id}",
            path_parameters: vec![Parameter {
                name: "entry_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read a library entry with its module rows.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<LibraryEntryResponse>(),
            },
        },
        Route {
            operation_id: "delete_library_entry",
            method: Method::Delete,
            path: "/api/surface-library/{entry_id}",
            path_parameters: vec![Parameter {
                name: "entry_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Remove an entry from the library. Surfaces seeded from it are untouched.",
            request_optional: false,

            request: None,
            response: Response {
                status: 204,
                body: None,
            },
        },
        Route {
            operation_id: "save_surface_to_library",
            method: Method::Post,
            path: "/api/surfaces/{surface_id}/save",
            path_parameters: vec![Parameter {
                name: "surface_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Preserve a surface in the shared library with a name, a description of what it does and when to use it, and tags.",
            request_optional: false,

            request: schema::<SaveSurfaceToLibraryRequest>(),
            response: Response {
                status: 200,
                body: schema::<LibraryEntryResponse>(),
            },
        },
        Route {
            operation_id: "list_topic_default_surfaces",
            method: Method::Get,
            path: "/api/topics/{topic_id}/default-surfaces",
            path_parameters: vec![Parameter {
                name: "topic_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "The library entries a topic opens in each of its new sessions.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<LibraryListResponse>(),
            },
        },
        Route {
            operation_id: "add_topic_default_surface",
            method: Method::Post,
            path: "/api/topics/{topic_id}/default-surfaces",
            path_parameters: vec![Parameter {
                name: "topic_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Designate a library entry as one of a topic's default surfaces.",
            request_optional: false,

            request: schema::<TopicDefaultSurfaceRequest>(),
            response: Response {
                status: 200,
                body: schema::<LibraryListResponse>(),
            },
        },
        Route {
            operation_id: "remove_topic_default_surface",
            method: Method::Delete,
            path: "/api/topics/{topic_id}/default-surfaces/{entry_id}",
            path_parameters: vec![
                Parameter {
                    name: "topic_id",
                    kind: ParameterType::Uuid,
                },
                Parameter {
                    name: "entry_id",
                    kind: ParameterType::Uuid,
                },
            ],
            query_parameters: vec![],
            summary: "Withdraw a library entry from a topic's default surfaces.",
            request_optional: false,

            request: None,
            response: Response {
                status: 204,
                body: None,
            },
        },
        Route {
            operation_id: "get_surface",
            method: Method::Get,
            path: "/api/surfaces/{surface_id}",
            path_parameters: vec![Parameter {
                name: "surface_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read a surface.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<SurfaceResponse>(),
            },
        },
        Route {
            operation_id: "write_surface_module",
            method: Method::Post,
            path: "/api/surfaces/{surface_id}/modules",
            path_parameters: vec![Parameter {
                name: "surface_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Write a surface module: append one, or replace one's source against the revision read — a moved-on revision is refused with the module as it now stands.",
            request_optional: false,

            request: schema::<WriteSurfaceModuleRequest>(),
            response: Response {
                status: 200,
                body: schema::<WriteSurfaceModuleResponse>(),
            },
        },
        Route {
            operation_id: "delete_surface_module",
            method: Method::Delete,
            path: "/api/surfaces/{surface_id}/modules/{module_id}",
            path_parameters: vec![
                Parameter {
                    name: "surface_id",
                    kind: ParameterType::Uuid,
                },
                Parameter {
                    name: "module_id",
                    kind: ParameterType::Uuid,
                },
            ],
            query_parameters: vec![],
            summary: "Remove a module from a surface, with the notes hung on it; both survive in the surface's history.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<DeleteSurfaceModuleResponse>(),
            },
        },
        Route {
            operation_id: "move_surface_module",
            method: Method::Post,
            path: "/api/surfaces/{surface_id}/modules/{module_id}/position",
            path_parameters: vec![
                Parameter {
                    name: "surface_id",
                    kind: ParameterType::Uuid,
                },
                Parameter {
                    name: "module_id",
                    kind: ParameterType::Uuid,
                },
            ],
            query_parameters: vec![],
            summary: "Place a module at a position among its surface's modules (0 first; past the end is last). Answers with the surface in its new order.",
            request_optional: false,

            request: schema::<MoveSurfaceModuleRequest>(),
            response: Response {
                status: 200,
                body: schema::<SurfaceResponse>(),
            },
        },
        Route {
            operation_id: "create_surface_annotation",
            method: Method::Post,
            path: "/api/surfaces/{surface_id}/annotations",
            path_parameters: vec![Parameter {
                name: "surface_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Create a surface annotation.",
            request_optional: false,

            request: schema::<CreateSurfaceAnnotationRequest>(),
            response: Response {
                status: 200,
                body: schema::<SurfaceAnnotationView>(),
            },
        },
        Route {
            operation_id: "update_surface_annotation",
            method: Method::Post,
            path: "/api/surface-annotations/{annotation_id}",
            path_parameters: vec![Parameter {
                name: "annotation_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Update a surface annotation.",
            request_optional: false,

            request: schema::<UpdateSurfaceAnnotationRequest>(),
            response: Response {
                status: 200,
                body: schema::<SurfaceAnnotationView>(),
            },
        },
        Route {
            operation_id: "delete_surface_annotation",
            method: Method::Delete,
            path: "/api/surface-annotations/{annotation_id}",
            path_parameters: vec![Parameter {
                name: "annotation_id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Delete a surface annotation.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<DeleteSurfaceAnnotationResponse>(),
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
            operation_id: "access_status",
            method: Method::Get,
            path: "/api/access",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Whether the gateway gates this connection and whether it is admitted.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<AccessStatusResponse>(),
            },
        },
        Route {
            operation_id: "issue_pairing_code",
            method: Method::Post,
            path: "/api/pair/codes",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Issue a one-time pairing code; loopback only.",
            request_optional: true,

            request: None,
            response: Response {
                status: 201,
                body: schema::<PairingCodeResponse>(),
            },
        },
        Route {
            operation_id: "pair_client",
            method: Method::Post,
            path: "/api/pair",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Redeem a pairing code for the gateway's token, set as the browser's cookie.",
            request_optional: false,

            request: schema::<PairClientRequest>(),
            response: Response {
                status: 200,
                body: schema::<PairClientResponse>(),
            },
        },
        Route {
            operation_id: "get_install_security_settings",
            method: Method::Get,
            path: "/api/settings/security",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Read the install's security checks: effective, overrides and relaxations.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<SecuritySettingsResponse>(),
            },
        },
        Route {
            operation_id: "save_install_security_settings",
            method: Method::Put,
            path: "/api/settings/security",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "Replace the install's security overrides.",
            request_optional: false,

            request: schema::<SaveSecuritySettingsRequest>(),
            response: Response {
                status: 200,
                body: schema::<SecuritySettingsResponse>(),
            },
        },
        Route {
            operation_id: "get_topic_security_settings",
            method: Method::Get,
            path: "/api/topics/{id}/settings/security",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read a topic's security checks: effective, overrides and relaxations.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<SecuritySettingsResponse>(),
            },
        },
        Route {
            operation_id: "save_topic_security_settings",
            method: Method::Put,
            path: "/api/topics/{id}/settings/security",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Replace a topic's security overrides.",
            request_optional: false,

            request: schema::<SaveSecuritySettingsRequest>(),
            response: Response {
                status: 200,
                body: schema::<SecuritySettingsResponse>(),
            },
        },
        Route {
            operation_id: "conversation_security_settings",
            method: Method::Get,
            path: "/api/conversations/{id}/security",
            path_parameters: vec![Parameter {
                name: "id",
                kind: ParameterType::Uuid,
            }],
            query_parameters: vec![],
            summary: "Read the security checks a conversation's session runs under.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<SecuritySettingsResponse>(),
            },
        },
        Route {
            operation_id: "list_documents",
            method: Method::Get,
            path: "/api/docs",
            path_parameters: vec![],
            query_parameters: vec![],
            summary: "List the platform's own documentation, hosted by the gateway.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<DocumentListResponse>(),
            },
        },
        Route {
            operation_id: "get_document",
            method: Method::Get,
            path: "/api/docs/{name}",
            path_parameters: vec![Parameter {
                name: "name",
                kind: ParameterType::String,
            }],
            query_parameters: vec![],
            summary: "Read one hosted document by name.",
            request_optional: false,

            request: None,
            response: Response {
                status: 200,
                body: schema::<DocumentResponse>(),
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
            .find(|route| route.operation_id == "get_topic")
            .unwrap();
        assert_eq!(get.probe_path(), "/api/topics/not-a-uuid");
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
