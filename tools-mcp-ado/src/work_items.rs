use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::io::ErrorKind;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;
use tools_mcp_core::ToolCallOutcome;
use url::Url;

const API_VERSION: &str = "7.1";
/// Default Azure CLI token audience: the well-known Azure DevOps application ID.
/// Callers may override it per request via the `resource` argument; it is never
/// read from the process environment.
const DEFAULT_AZURE_DEVOPS_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";
const USER_AGENT_VALUE: &str = "tools-mcp-ado/0.1";
const DEFAULT_TOP: usize = 20;
const MAX_TOP: usize = 100;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const AZ_CLI_TIMEOUT_MS: u64 = 10_000;
const MAX_ERROR_BODY_CHARS: usize = 2_000;
const MAX_DESCRIPTION_CHARS: usize = 8_000;

const SUMMARY_FIELDS: &[&str] = &[
    "System.Id",
    "System.TeamProject",
    "System.WorkItemType",
    "System.Title",
    "System.State",
    "System.AssignedTo",
    "System.CreatedBy",
    "System.ChangedBy",
    "System.CreatedDate",
    "System.ChangedDate",
    "System.AreaPath",
    "System.IterationPath",
    "System.Tags",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdoWorkItemsRequest {
    organization: Option<String>,
    project: Option<String>,
    #[serde(alias = "number")]
    id: Option<u64>,
    keyword: Option<String>,
    assigned_to: Option<String>,
    state: Option<String>,
    work_item_type: Option<String>,
    top: Option<usize>,
    include_description: Option<bool>,
    timeout_ms: Option<u64>,
    /// Azure CLI access-token audience. Must be a GUID or an https URL.
    resource: Option<String>,
}

#[derive(Debug, Serialize)]
struct AdoWorkItemsResponse {
    organization: String,
    project: String,
    resource: String,
    count: usize,
    lookup: LookupEcho,
    work_items: Vec<WorkItemSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct LookupEcho {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyword: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assigned_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_item_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top: Option<usize>,
}

#[derive(Debug, Serialize)]
struct WorkItemSummary {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_url: Option<String>,
    web_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_item_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assigned_to: Option<IdentitySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_by: Option<IdentitySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed_by: Option<IdentitySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    area_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iteration_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    description_truncated: bool,
}

#[derive(Debug, Serialize)]
struct IdentitySummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unique_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Debug)]
struct ResolvedContext {
    organization: String,
    project: String,
    resource: String,
    lookup: ResolvedLookup,
}

#[derive(Debug)]
struct ResolvedLookup {
    id: Option<u64>,
    keyword: Option<String>,
    assigned_to: Option<String>,
    state: Option<String>,
    work_item_type: Option<String>,
    top: usize,
    include_description: bool,
    timeout_ms: u64,
}

#[derive(Debug)]
enum AdoError {
    MissingOrganization,
    InvalidOrganization(String),
    MissingProject,
    InvalidProject(String),
    InvalidResource(String),
    AzureCliUnavailable,
    AzureCliFailed(String),
    AzureCliNoToken,
    InvalidArguments {
        error_type: &'static str,
        message: String,
    },
    Url(String),
    Header(String),
    Request(String),
    Decode(String),
    HttpStatus {
        status: u16,
        reason: String,
        body: String,
    },
}

pub(crate) async fn handle_ado_work_items(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let request = match ToolCallOutcome::parse_args::<AdoWorkItemsRequest>(&args) {
        Ok(request) => request,
        Err(outcome) => return outcome,
    };

    let context = match resolve_context(request) {
        Ok(context) => context,
        Err(error) => return ado_error_outcome(error, None, None),
    };

    let organization = context.organization.clone();
    let project = context.project.clone();

    match execute_lookup(context).await {
        Ok(response) => ado_success_outcome(response),
        Err(error) => ado_error_outcome(error, Some(&organization), Some(&project)),
    }
}

async fn execute_lookup(context: ResolvedContext) -> Result<AdoWorkItemsResponse, AdoError> {
    let token = azure_cli_access_token(&context.resource).await?;
    let authorization = bearer_auth_header(&token)?;
    let client = ado_client(context.lookup.timeout_ms)?;

    let work_items = if let Some(id) = context.lookup.id {
        vec![
            get_work_item(&client, &context, &authorization, id)
                .await?
                .into_summary(
                    &context.organization,
                    &context.project,
                    context.lookup.include_description,
                )?,
        ]
    } else {
        search_work_items(&client, &context, &authorization).await?
    };

    let lookup = LookupEcho {
        id: context.lookup.id,
        keyword: context.lookup.keyword,
        assigned_to: context.lookup.assigned_to,
        state: context.lookup.state,
        work_item_type: context.lookup.work_item_type,
        top: context.lookup.id.is_none().then_some(context.lookup.top),
    };

    Ok(AdoWorkItemsResponse {
        organization: context.organization,
        project: context.project,
        resource: context.resource,
        count: work_items.len(),
        lookup,
        work_items,
    })
}

async fn search_work_items(
    client: &reqwest::Client,
    context: &ResolvedContext,
    authorization: &HeaderValue,
) -> Result<Vec<WorkItemSummary>, AdoError> {
    let wiql = build_wiql(&context.project, &context.lookup);
    let url = wiql_url(&context.organization, &context.project, context.lookup.top)?;
    let wiql_response: WiqlResponse =
        post_json(client, url, authorization, &WiqlRequest { query: wiql }).await?;

    let ids: Vec<u64> = wiql_response
        .work_items
        .into_iter()
        .map(|work_item| work_item.id)
        .collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let batch = get_work_items_batch(client, context, authorization, ids).await?;
    batch
        .into_iter()
        .map(|item| {
            item.into_summary(
                &context.organization,
                &context.project,
                context.lookup.include_description,
            )
        })
        .collect()
}

async fn get_work_item(
    client: &reqwest::Client,
    context: &ResolvedContext,
    authorization: &HeaderValue,
    id: u64,
) -> Result<RawWorkItem, AdoError> {
    let url = work_item_url(&context.organization, &context.project, id)?;
    get_json(client, url, authorization).await
}

async fn get_work_items_batch(
    client: &reqwest::Client,
    context: &ResolvedContext,
    authorization: &HeaderValue,
    ids: Vec<u64>,
) -> Result<Vec<RawWorkItem>, AdoError> {
    let url = batch_url(&context.organization, &context.project)?;
    let body = WorkItemsBatchRequest {
        ids,
        fields: requested_fields(context.lookup.include_description),
        expand: "Fields",
    };
    let response: WorkItemsBatchResponse = post_json(client, url, authorization, &body).await?;
    Ok(response.value)
}

fn resolve_context(request: AdoWorkItemsRequest) -> Result<ResolvedContext, AdoError> {
    let organization = match request.organization.as_deref() {
        None => Err(AdoError::MissingOrganization),
        Some(value) if value.trim().is_empty() => {
            Err(AdoError::InvalidOrganization(value.to_string()))
        }
        Some(value) => normalize_organization(value.to_string()),
    }?;

    let project = match request.project.as_deref() {
        None => Err(AdoError::MissingProject),
        Some(value) if value.trim().is_empty() => Err(AdoError::InvalidProject(value.to_string())),
        Some(value) => validate_project(value.to_string()),
    }?;

    let resource = match request.resource.as_deref() {
        None => DEFAULT_AZURE_DEVOPS_RESOURCE.to_string(),
        Some(value) => validate_resource(value.to_string())?,
    };

    let lookup = resolve_lookup(request)?;

    Ok(ResolvedContext {
        organization,
        project,
        resource,
        lookup,
    })
}

fn resolve_lookup(request: AdoWorkItemsRequest) -> Result<ResolvedLookup, AdoError> {
    let keyword = optional_non_empty("keyword", request.keyword)?;
    let assigned_to = optional_non_empty("assigned_to", request.assigned_to)?;
    let state = optional_non_empty("state", request.state)?;
    let work_item_type = optional_non_empty("work_item_type", request.work_item_type)?;

    if matches!(request.id, Some(0)) {
        return Err(invalid_arguments(
            "invalid_id",
            "id must be greater than or equal to 1.",
        ));
    }

    let has_search_selector = keyword.is_some() || assigned_to.is_some();
    let has_any_filter = has_search_selector || state.is_some() || work_item_type.is_some();
    if request.id.is_some() && has_any_filter {
        return Err(invalid_arguments(
            "invalid_lookup",
            "id/number is an exact lookup and cannot be combined with keyword, assigned_to, state, or work_item_type.",
        ));
    }
    if request.id.is_none() && !has_search_selector {
        return Err(invalid_arguments(
            "selector_required",
            "Provide one of id, number, keyword, or assigned_to.",
        ));
    }

    let top = request.top.unwrap_or(DEFAULT_TOP);
    if !(1..=MAX_TOP).contains(&top) {
        return Err(invalid_arguments(
            "invalid_top",
            format!("top must be between 1 and {MAX_TOP}."),
        ));
    }

    let timeout_ms = request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if !(100..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(invalid_arguments(
            "invalid_timeout",
            format!("timeout_ms must be between 100 and {MAX_TIMEOUT_MS}."),
        ));
    }

    Ok(ResolvedLookup {
        id: request.id,
        keyword,
        assigned_to,
        state,
        work_item_type,
        top,
        include_description: request.include_description.unwrap_or(false),
        timeout_ms,
    })
}

fn ado_success_outcome(response: AdoWorkItemsResponse) -> ToolCallOutcome {
    let text = summary_text(&response);
    ToolCallOutcome::ok(json!({
        "content": [{"type": "text", "text": text}],
        "isError": false,
        "organization": response.organization,
        "project": response.project,
        "resource": response.resource,
        "count": response.count,
        "lookup": response.lookup,
        "work_items": response.work_items,
    }))
}

fn ado_error_outcome(
    error: AdoError,
    organization: Option<&str>,
    project: Option<&str>,
) -> ToolCallOutcome {
    let mut fields = vec![
        ("error_type", json!(error.error_type())),
        ("remediation", json!(error.remediation())),
    ];
    if let Some(organization) = organization {
        fields.push(("organization", json!(organization)));
    }
    if let Some(project) = project {
        fields.push(("project", json!(project)));
    }
    if let AdoError::HttpStatus {
        status,
        reason,
        body,
    } = &error
    {
        fields.push(("status", json!(status)));
        fields.push(("reason", json!(reason)));
        if !body.is_empty() {
            fields.push(("details", json!(body)));
        }
    }

    ToolCallOutcome::err_with(error.message(), fields)
}

impl AdoError {
    fn message(&self) -> String {
        match self {
            Self::MissingOrganization => {
                "Azure DevOps organization is required. Provide the `organization` argument."
                    .to_string()
            }
            Self::InvalidOrganization(value) => format!(
                "Invalid Azure DevOps organization `{value}`. Use an organization slug or a dev.azure.com/{value} URL."
            ),
            Self::MissingProject => {
                "Azure DevOps project is required. Provide the `project` argument.".to_string()
            }
            Self::InvalidProject(value) => {
                format!(
                    "Invalid Azure DevOps project `{value}`. Project names cannot be empty or contain path separators."
                )
            }
            Self::InvalidResource(value) => {
                format!(
                    "Invalid `resource` `{value}`. Use an Azure application ID (GUID) or an https URL audience."
                )
            }
            Self::AzureCliUnavailable => {
                "Azure CLI (`az`) is not available on the host running the MCP server.".to_string()
            }
            Self::AzureCliFailed(message) => {
                format!("Azure CLI could not provide an Azure DevOps access token: {message}")
            }
            Self::AzureCliNoToken => "Azure CLI returned no Azure DevOps access token.".to_string(),
            Self::InvalidArguments { message, .. } => message.clone(),
            Self::Url(message) => format!("Failed to build Azure DevOps API URL: {message}"),
            Self::Header(message) => {
                format!("Failed to build Azure DevOps request headers: {message}")
            }
            Self::Request(message) => format!("Azure DevOps request failed: {message}"),
            Self::Decode(message) => {
                format!("Azure DevOps returned an unexpected response: {message}")
            }
            Self::HttpStatus { status, reason, .. } => {
                format!("Azure DevOps returned HTTP {status} {reason}.")
            }
        }
    }

    fn error_type(&self) -> &'static str {
        match self {
            Self::MissingOrganization => "missing_organization",
            Self::InvalidOrganization(_) => "invalid_organization",
            Self::MissingProject => "missing_project",
            Self::InvalidProject(_) => "invalid_project",
            Self::InvalidResource(_) => "invalid_resource",
            Self::AzureCliUnavailable => "azure_cli_unavailable",
            Self::AzureCliFailed(_) => "azure_cli_auth_failed",
            Self::AzureCliNoToken => "azure_cli_no_token",
            Self::InvalidArguments { error_type, .. } => error_type,
            Self::Url(_) => "url_error",
            Self::Header(_) => "header_error",
            Self::Request(_) => "request_error",
            Self::Decode(_) => "decode_error",
            Self::HttpStatus { status, .. } if *status == 401 || *status == 403 => "auth_failed",
            Self::HttpStatus { status, .. } if *status == 404 => "not_found",
            Self::HttpStatus { .. } => "http_error",
        }
    }

    fn remediation(&self) -> Vec<String> {
        match self {
            Self::MissingOrganization | Self::InvalidOrganization(_) => vec![
                "Pass `organization` as an Azure DevOps Services org slug, for example `contoso`.".to_string(),
                "A dev.azure.com/{org} or {org}.visualstudio.com URL is also accepted.".to_string(),
            ],
            Self::MissingProject | Self::InvalidProject(_) => vec![
                "Pass the Azure DevOps project name in `project`.".to_string(),
            ],
            Self::InvalidResource(_) => vec![
                "Omit `resource` to use the default Azure DevOps audience, or pass an Azure application ID (GUID) or https URL.".to_string(),
            ],
            Self::AzureCliUnavailable => vec![
                "Install Azure CLI (`az`) on the host running the MCP server.".to_string(),
                format!("The tool runs: az account get-access-token --resource {DEFAULT_AZURE_DEVOPS_RESOURCE} --query accessToken -o tsv"),
            ],
            Self::AzureCliFailed(_) | Self::AzureCliNoToken => vec![
                "Run `az login` for an account that can read the target Azure DevOps project, then retry.".to_string(),
                format!("Verify auth with: az account get-access-token --resource {DEFAULT_AZURE_DEVOPS_RESOURCE} --query accessToken -o tsv"),
            ],
            Self::HttpStatus { status: 401 | 403, .. } => vec![
                "Authenticate Azure CLI with an account that has Work Items read access for the target organization/project.".to_string(),
            ],
            Self::InvalidArguments { .. } => vec![
                "Use id/number for one exact work item, or keyword and/or assigned_to for search.".to_string(),
            ],
            Self::HttpStatus { status: 404, .. } => vec![
                "Check that the organization, project, and work item ID are correct.".to_string(),
                "Confirm the signed-in account has access to the target project.".to_string(),
            ],
            Self::Request(_) | Self::HttpStatus { .. } => vec![
                "Retry later if Azure DevOps is temporarily unavailable.".to_string(),
                "Check network connectivity from the host running the MCP server.".to_string(),
            ],
            Self::Url(_) | Self::Header(_) | Self::Decode(_) => vec![
                "Check server logs for diagnostic detail if you operate this MCP server.".to_string(),
            ],
        }
    }
}

fn ado_client(timeout_ms: u64) -> Result<reqwest::Client, AdoError> {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .default_headers(default_headers())
        .build()
        .map_err(|error| AdoError::Request(error.to_string()))
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers
}

async fn get_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: Url,
    authorization: &HeaderValue,
) -> Result<T, AdoError> {
    let response = client
        .get(url)
        .header(AUTHORIZATION, authorization.clone())
        .send()
        .await
        .map_err(|error| AdoError::Request(error.to_string()))?;
    decode_response(response).await
}

async fn post_json<T: DeserializeOwned, B: Serialize>(
    client: &reqwest::Client,
    url: Url,
    authorization: &HeaderValue,
    body: &B,
) -> Result<T, AdoError> {
    let response = client
        .post(url)
        .header(AUTHORIZATION, authorization.clone())
        .json(body)
        .send()
        .await
        .map_err(|error| AdoError::Request(error.to_string()))?;
    decode_response(response).await
}

async fn decode_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, AdoError> {
    let status = response.status();
    let reason = status.canonical_reason().unwrap_or("").to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AdoError::Request(error.to_string()))?;

    if !status.is_success() {
        return Err(AdoError::HttpStatus {
            status: status.as_u16(),
            reason,
            body: truncate_chars(&String::from_utf8_lossy(&bytes), MAX_ERROR_BODY_CHARS),
        });
    }

    serde_json::from_slice(&bytes).map_err(|error| AdoError::Decode(error.to_string()))
}

/// Obtain a short-lived Azure DevOps access token from Azure CLI.
///
/// The command is invoked with a fixed argument vector (no shell), so the
/// caller-supplied `resource` cannot inject additional flags or commands.
async fn azure_cli_access_token(resource: &str) -> Result<String, AdoError> {
    let mut command = Command::new(azure_cli_binary());
    command
        .arg("account")
        .arg("get-access-token")
        .arg("--resource")
        .arg(resource)
        .arg("--query")
        .arg("accessToken")
        .arg("-o")
        .arg("tsv")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());

    let output =
        match time::timeout(Duration::from_millis(AZ_CLI_TIMEOUT_MS), command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) if error.kind() == ErrorKind::NotFound => {
                return Err(AdoError::AzureCliUnavailable);
            }
            Ok(Err(error)) => return Err(AdoError::AzureCliFailed(error.to_string())),
            Err(_) => {
                return Err(AdoError::AzureCliFailed(format!(
                    "Azure CLI token command timed out after {AZ_CLI_TIMEOUT_MS} ms"
                )));
            }
        };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let details = truncate_chars(stderr.trim(), MAX_ERROR_BODY_CHARS);
        let message = if details.is_empty() {
            format!(
                "Azure CLI token command exited with status {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            )
        } else {
            details
        };
        return Err(AdoError::AzureCliFailed(message));
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        Err(AdoError::AzureCliNoToken)
    } else {
        Ok(token)
    }
}

fn azure_cli_binary() -> &'static str {
    if cfg!(target_os = "windows") {
        "az.cmd"
    } else {
        "az"
    }
}

fn bearer_auth_header(token: &str) -> Result<HeaderValue, AdoError> {
    HeaderValue::from_str(&format!("Bearer {}", token.trim())).map_err(|error| {
        AdoError::Header(format!(
            "authorization header value could not be constructed: {error}"
        ))
    })
}

fn build_wiql(project: &str, lookup: &ResolvedLookup) -> String {
    let mut filters = vec![format!(
        "[System.TeamProject] = '{}'",
        escape_wiql_string(project)
    )];

    if let Some(keyword) = &lookup.keyword {
        let value = escape_wiql_string(keyword);
        filters.push(format!(
            "([System.Title] CONTAINS WORDS '{value}' OR [System.Description] CONTAINS WORDS '{value}' OR [System.Tags] CONTAINS '{value}')"
        ));
    }
    if let Some(assigned_to) = &lookup.assigned_to {
        filters.push(format!(
            "[System.AssignedTo] = '{}'",
            escape_wiql_string(assigned_to)
        ));
    }
    if let Some(state) = &lookup.state {
        filters.push(format!("[System.State] = '{}'", escape_wiql_string(state)));
    }
    if let Some(work_item_type) = &lookup.work_item_type {
        filters.push(format!(
            "[System.WorkItemType] = '{}'",
            escape_wiql_string(work_item_type)
        ));
    }

    format!(
        "SELECT [System.Id] FROM WorkItems WHERE {} ORDER BY [System.ChangedDate] DESC",
        filters.join(" AND ")
    )
}

fn work_item_url(organization: &str, project: &str, id: u64) -> Result<Url, AdoError> {
    ado_url(
        organization,
        project,
        &["_apis", "wit", "workitems", &id.to_string()],
        &[("api-version", API_VERSION)],
    )
}

fn wiql_url(organization: &str, project: &str, top: usize) -> Result<Url, AdoError> {
    ado_url(
        organization,
        project,
        &["_apis", "wit", "wiql"],
        &[("api-version", API_VERSION), ("$top", &top.to_string())],
    )
}

fn batch_url(organization: &str, project: &str) -> Result<Url, AdoError> {
    ado_url(
        organization,
        project,
        &["_apis", "wit", "workitemsbatch"],
        &[("api-version", API_VERSION)],
    )
}

fn ado_url(
    organization: &str,
    project: &str,
    path_segments: &[&str],
    query_pairs: &[(&str, &str)],
) -> Result<Url, AdoError> {
    let mut url =
        Url::parse("https://dev.azure.com/").map_err(|error| AdoError::Url(error.to_string()))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| AdoError::Url("base URL cannot be a base".to_string()))?;
        segments.push(organization);
        segments.push(project);
        for segment in path_segments {
            segments.push(segment);
        }
    }
    {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in query_pairs {
            pairs.append_pair(name, value);
        }
    }
    Ok(url)
}

#[derive(Debug, Serialize)]
struct WiqlRequest {
    query: String,
}

#[derive(Debug, Deserialize)]
struct WiqlResponse {
    #[serde(default, rename = "workItems")]
    work_items: Vec<WiqlWorkItem>,
}

#[derive(Debug, Deserialize)]
struct WiqlWorkItem {
    id: u64,
}

#[derive(Debug, Serialize)]
struct WorkItemsBatchRequest {
    ids: Vec<u64>,
    fields: Vec<&'static str>,
    #[serde(rename = "$expand")]
    expand: &'static str,
}

#[derive(Debug, Deserialize)]
struct WorkItemsBatchResponse {
    #[serde(default)]
    value: Vec<RawWorkItem>,
}

#[derive(Debug, Deserialize)]
struct RawWorkItem {
    id: u64,
    rev: Option<u64>,
    url: Option<String>,
    #[serde(default)]
    fields: Map<String, Value>,
}

impl RawWorkItem {
    fn into_summary(
        self,
        organization: &str,
        project: &str,
        include_description: bool,
    ) -> Result<WorkItemSummary, AdoError> {
        let (description, description_truncated) = if include_description {
            limited_field_string(&self.fields, "System.Description", MAX_DESCRIPTION_CHARS)
        } else {
            (None, false)
        };

        Ok(WorkItemSummary {
            id: self.id,
            rev: self.rev,
            api_url: self.url,
            web_url: work_item_web_url(organization, project, self.id)?,
            project: field_string(&self.fields, "System.TeamProject"),
            work_item_type: field_string(&self.fields, "System.WorkItemType"),
            title: field_string(&self.fields, "System.Title"),
            state: field_string(&self.fields, "System.State"),
            assigned_to: field_identity(&self.fields, "System.AssignedTo"),
            created_by: field_identity(&self.fields, "System.CreatedBy"),
            changed_by: field_identity(&self.fields, "System.ChangedBy"),
            created_date: field_string(&self.fields, "System.CreatedDate"),
            changed_date: field_string(&self.fields, "System.ChangedDate"),
            area_path: field_string(&self.fields, "System.AreaPath"),
            iteration_path: field_string(&self.fields, "System.IterationPath"),
            tags: field_string(&self.fields, "System.Tags")
                .map(|tags| {
                    tags.split(';')
                        .filter_map(non_empty_trimmed)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            description,
            description_truncated,
        })
    }
}

fn requested_fields(include_description: bool) -> Vec<&'static str> {
    let mut fields = SUMMARY_FIELDS.to_vec();
    if include_description {
        fields.push("System.Description");
    }
    fields
}

fn work_item_web_url(organization: &str, project: &str, id: u64) -> Result<String, AdoError> {
    ado_url(
        organization,
        project,
        &["_workitems", "edit", &id.to_string()],
        &[],
    )
    .map(|url| url.to_string())
}

fn field_string(fields: &Map<String, Value>, field_name: &str) -> Option<String> {
    fields.get(field_name).and_then(value_to_string)
}

fn limited_field_string(
    fields: &Map<String, Value>,
    field_name: &str,
    max_chars: usize,
) -> (Option<String>, bool) {
    let Some(value) = field_string(fields, field_name) else {
        return (None, false);
    };
    let (truncated, was_truncated) = truncate_chars_with_flag(&value, max_chars);
    (Some(truncated), was_truncated)
}

fn field_identity(fields: &Map<String, Value>, field_name: &str) -> Option<IdentitySummary> {
    let value = fields.get(field_name)?;
    match value {
        Value::String(display_name) => Some(IdentitySummary {
            display_name: Some(display_name.clone()),
            unique_name: None,
            id: None,
        }),
        Value::Object(object) => Some(IdentitySummary {
            display_name: object
                .get("displayName")
                .and_then(value_to_string)
                .or_else(|| object.get("name").and_then(value_to_string)),
            unique_name: object
                .get("uniqueName")
                .and_then(value_to_string)
                .or_else(|| object.get("mailAddress").and_then(value_to_string)),
            id: object.get("id").and_then(value_to_string),
        }),
        _ => value_to_string(value).map(|display_name| IdentitySummary {
            display_name: Some(display_name),
            unique_name: None,
            id: None,
        }),
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => non_empty_trimmed(value),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn summary_text(response: &AdoWorkItemsResponse) -> String {
    if response.work_items.is_empty() {
        return format!(
            "No Azure DevOps work items found in {}/{}.",
            response.organization, response.project
        );
    }

    let mut lines = vec![format!(
        "Found {} Azure DevOps work item(s) in {}/{}.",
        response.count, response.organization, response.project
    )];
    for item in &response.work_items {
        let kind = item.work_item_type.as_deref().unwrap_or("Work Item");
        let state = item.state.as_deref().unwrap_or("unknown");
        let title = item.title.as_deref().unwrap_or("(untitled)");
        lines.push(format!(
            "#{} [{}] {} - {} ({})",
            item.id, kind, state, title, item.web_url
        ));
    }
    lines.join("\n")
}

fn normalize_organization(value: String) -> Result<String, AdoError> {
    let raw = value.trim().trim_end_matches('/');
    let candidate = if raw.starts_with("https://") || raw.starts_with("http://") {
        let url = Url::parse(raw).map_err(|_| AdoError::InvalidOrganization(value.clone()))?;
        if url.scheme() != "https" {
            return Err(AdoError::InvalidOrganization(value));
        }
        let host = url
            .host_str()
            .ok_or_else(|| AdoError::InvalidOrganization(value.clone()))?;
        if host.eq_ignore_ascii_case("dev.azure.com") {
            url.path_segments()
                .and_then(|mut segments| segments.find(|segment| !segment.is_empty()))
                .ok_or_else(|| AdoError::InvalidOrganization(value.clone()))?
                .to_string()
        } else if let Some(org) = host.strip_suffix(".visualstudio.com") {
            org.to_string()
        } else {
            return Err(AdoError::InvalidOrganization(value));
        }
    } else {
        raw.to_string()
    };

    if is_valid_organization_slug(&candidate) {
        Ok(candidate)
    } else {
        Err(AdoError::InvalidOrganization(value))
    }
}

fn validate_project(value: String) -> Result<String, AdoError> {
    let project = value.trim();
    if project.is_empty() || project.contains('/') || project.contains('\\') {
        return Err(AdoError::InvalidProject(value));
    }
    Ok(project.to_string())
}

/// Validate the Azure CLI token audience. Accepts an Azure application ID (GUID)
/// or an https URL; rejects everything else so an attacker-influenced value
/// cannot be passed verbatim to the CLI.
fn validate_resource(value: String) -> Result<String, AdoError> {
    let resource = value.trim();
    if is_guid(resource) {
        return Ok(resource.to_string());
    }
    if let Ok(url) = Url::parse(resource)
        && url.scheme() == "https"
        && url.host_str().is_some()
    {
        return Ok(resource.to_string());
    }
    Err(AdoError::InvalidResource(value))
}

fn is_guid(value: &str) -> bool {
    const GROUP_LENGTHS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != GROUP_LENGTHS.len() {
        return false;
    }
    parts
        .iter()
        .zip(GROUP_LENGTHS)
        .all(|(part, length)| part.len() == length && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_valid_organization_slug(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let last = value.chars().last().unwrap_or(first);
    last.is_ascii_alphanumeric()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn optional_non_empty(
    name: &'static str,
    value: Option<String>,
) -> Result<Option<String>, AdoError> {
    match value {
        Some(value) => non_empty_trimmed(&value).map(Some).ok_or_else(|| {
            invalid_arguments(
                "invalid_argument",
                format!("{name} must not be empty when provided."),
            )
        }),
        None => Ok(None),
    }
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn escape_wiql_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    truncate_chars_with_flag(value, max_chars).0
}

fn truncate_chars_with_flag(value: &str, max_chars: usize) -> (String, bool) {
    let mut truncated = String::new();
    for (index, character) in value.chars().enumerate() {
        if index == max_chars {
            truncated.push_str("...");
            return (truncated, true);
        }
        truncated.push(character);
    }
    (truncated, false)
}

fn invalid_arguments(error_type: &'static str, message: impl Into<String>) -> AdoError {
    AdoError::InvalidArguments {
        error_type,
        message: message.into(),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> AdoWorkItemsRequest {
        AdoWorkItemsRequest {
            organization: Some("contoso".to_string()),
            project: Some("Tools".to_string()),
            id: None,
            keyword: None,
            assigned_to: None,
            state: None,
            work_item_type: None,
            top: None,
            include_description: None,
            timeout_ms: None,
            resource: None,
        }
    }

    #[test]
    fn normalize_organization_accepts_slug_and_urls() {
        assert_eq!(
            normalize_organization("contoso".to_string()).expect("slug should normalize"),
            "contoso"
        );
        assert_eq!(
            normalize_organization("https://dev.azure.com/contoso/".to_string())
                .expect("dev.azure.com URL should normalize"),
            "contoso"
        );
        assert_eq!(
            normalize_organization("https://contoso.visualstudio.com/".to_string())
                .expect("visualstudio.com URL should normalize"),
            "contoso"
        );
    }

    #[test]
    fn normalize_organization_rejects_untrusted_hosts_and_paths() {
        assert!(normalize_organization("https://example.com/contoso".to_string()).is_err());
        assert!(normalize_organization("contoso/project".to_string()).is_err());
        assert!(normalize_organization("-contoso".to_string()).is_err());
    }

    #[test]
    fn resolve_context_requires_organization_and_project() {
        let mut request = base_request();
        request.organization = None;
        request.id = Some(123);
        assert_eq!(
            resolve_context(request)
                .expect_err("missing organization should fail")
                .error_type(),
            "missing_organization"
        );

        let mut request = base_request();
        request.project = None;
        request.id = Some(123);
        assert_eq!(
            resolve_context(request)
                .expect_err("missing project should fail")
                .error_type(),
            "missing_project"
        );
    }

    #[test]
    fn resolve_context_requires_a_lookup_selector() {
        let error = resolve_context(base_request()).expect_err("missing selector should fail");
        assert_eq!(error.error_type(), "selector_required");
    }

    #[test]
    fn resolve_context_defaults_resource_to_azure_devops() {
        let mut request = base_request();
        request.id = Some(123);
        let context = resolve_context(request).expect("valid exact lookup should resolve");

        assert_eq!(context.organization, "contoso");
        assert_eq!(context.project, "Tools");
        assert_eq!(context.resource, DEFAULT_AZURE_DEVOPS_RESOURCE);
        assert_eq!(context.lookup.id, Some(123));
    }

    #[test]
    fn resolve_context_accepts_custom_resource_guid_and_https() {
        let mut request = base_request();
        request.id = Some(1);
        request.resource = Some("00000003-0000-0000-c000-000000000000".to_string());
        assert_eq!(
            resolve_context(request)
                .expect("guid resource should resolve")
                .resource,
            "00000003-0000-0000-c000-000000000000"
        );

        let mut request = base_request();
        request.id = Some(1);
        request.resource = Some("https://graph.microsoft.com/".to_string());
        assert_eq!(
            resolve_context(request)
                .expect("https resource should resolve")
                .resource,
            "https://graph.microsoft.com/"
        );
    }

    #[test]
    fn resolve_context_rejects_invalid_resource() {
        for bad in [
            "--cloud-name=AzureUSGovernment",
            "not-a-guid",
            "ftp://example.com",
            "499b84ac1321427faa17267ca6975798",
        ] {
            let mut request = base_request();
            request.id = Some(1);
            request.resource = Some(bad.to_string());
            assert_eq!(
                resolve_context(request)
                    .expect_err("invalid resource should fail")
                    .error_type(),
                "invalid_resource",
                "resource `{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn resolve_context_rejects_id_combined_with_filters() {
        let mut request = base_request();
        request.id = Some(5);
        request.keyword = Some("crash".to_string());
        assert_eq!(
            resolve_context(request)
                .expect_err("id + keyword should fail")
                .error_type(),
            "invalid_lookup"
        );
    }

    #[test]
    fn build_wiql_combines_filters_and_escapes_literals() {
        let lookup = ResolvedLookup {
            id: None,
            keyword: Some("can't save".to_string()),
            assigned_to: Some("alex@example.com".to_string()),
            state: Some("Active".to_string()),
            work_item_type: Some("Bug".to_string()),
            top: 10,
            include_description: false,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };

        let wiql = build_wiql("Tools", &lookup);

        assert!(wiql.contains("[System.TeamProject] = 'Tools'"));
        assert!(wiql.contains("CONTAINS WORDS 'can''t save'"));
        assert!(wiql.contains("[System.AssignedTo] = 'alex@example.com'"));
        assert!(wiql.contains("[System.State] = 'Active'"));
        assert!(wiql.contains("[System.WorkItemType] = 'Bug'"));
        assert!(wiql.ends_with("ORDER BY [System.ChangedDate] DESC"));
    }

    #[test]
    fn bearer_auth_header_uses_bearer_scheme() {
        let header = bearer_auth_header("access-token").expect("bearer header");
        assert_eq!(header.to_str().expect("bearer str"), "Bearer access-token");
    }

    #[test]
    fn is_guid_matches_only_canonical_guids() {
        assert!(is_guid(DEFAULT_AZURE_DEVOPS_RESOURCE));
        assert!(!is_guid("499b84ac1321427faa17267ca6975798"));
        assert!(!is_guid("zzzzzzzz-1321-427f-aa17-267ca6975798"));
    }

    #[test]
    fn raw_work_item_summary_handles_identity_objects_and_tags() {
        let raw = RawWorkItem {
            id: 42,
            rev: Some(3),
            url: Some("https://dev.azure.com/contoso/_apis/wit/workItems/42".to_string()),
            fields: Map::from_iter([
                ("System.TeamProject".to_string(), json!("Tools")),
                ("System.WorkItemType".to_string(), json!("Bug")),
                ("System.Title".to_string(), json!("Fix search")),
                ("System.State".to_string(), json!("Active")),
                (
                    "System.AssignedTo".to_string(),
                    json!({"displayName": "Alex Doe", "uniqueName": "alex@example.com", "id": "abc"}),
                ),
                ("System.Tags".to_string(), json!("bug; search ; ado")),
            ]),
        };

        let summary = raw
            .into_summary("contoso", "Tools", false)
            .expect("summary should build");

        assert_eq!(summary.id, 42);
        assert_eq!(summary.title.as_deref(), Some("Fix search"));
        assert_eq!(
            summary
                .assigned_to
                .as_ref()
                .and_then(|identity| identity.unique_name.as_deref()),
            Some("alex@example.com")
        );
        assert_eq!(summary.tags, vec!["bug", "search", "ado"]);
        assert!(summary.web_url.contains("/_workitems/edit/42"));
    }
}
