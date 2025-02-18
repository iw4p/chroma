use std::str::FromStr;

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router, ServiceExt,
};
use chroma_types::{
    operator::Filter, AddCollectionRecordsResponse, ChecklistResponse, Collection,
    CollectionMetadataUpdate, CollectionUuid, CountCollectionsRequest, CountCollectionsResponse,
    CountRequest, CountResponse, CreateCollectionRequest, CreateDatabaseRequest,
    CreateDatabaseResponse, CreateTenantRequest, CreateTenantResponse,
    DeleteCollectionRecordsResponse, DeleteDatabaseRequest, DeleteDatabaseResponse,
    GetCollectionRequest, GetDatabaseRequest, GetDatabaseResponse, GetRequest, GetResponse,
    GetTenantRequest, GetTenantResponse, GetUserIdentityResponse, HeartbeatResponse, IncludeList,
    ListCollectionsRequest, ListCollectionsResponse, ListDatabasesRequest, ListDatabasesResponse,
    Metadata, QueryRequest, QueryResponse, UpdateCollectionRecordsResponse,
    UpdateCollectionResponse, UpdateMetadata, UpsertCollectionRecordsResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ac::AdmissionControlledService,
    frontend::Frontend,
    tower_tracing::add_tracing_middleware,
    types::{
        errors::{ServerError, ValidationError},
        where_parsing::RawWhereFields,
    },
    utils::{validate_name, validate_non_empty_filter, validate_non_empty_metadata},
    FrontendConfig,
};

#[derive(Clone)]
pub(crate) struct FrontendServer {
    config: FrontendConfig,
    frontend: Frontend,
}

impl FrontendServer {
    pub fn new(config: FrontendConfig, frontend: Frontend) -> FrontendServer {
        FrontendServer { config, frontend }
    }

    #[allow(dead_code)]
    pub async fn run(server: FrontendServer) {
        let circuit_breaker_config = server.config.circuit_breaker.clone();
        let app = Router::new()
            // `GET /` goes to `root`
            .route("/api/v2", get(root))
            .route("/api/v2/heartbeat", get(heartbeat))
            .route("/api/v2/pre-flight-checks", get(pre_flight_checks))
            .route("/api/v2/reset", post(reset))
            .route("/api/v2/version", get(version))
            .route("/api/v2/auth/identity", get(get_user_identity))
            .route("/api/v2/tenants", post(create_tenant))
            .route("/api/v2/tenants/:tenant_name", get(get_tenant))
            .route("/api/v2/tenants/:tenant_id/databases", get(list_databases).post(create_database))
            .route("/api/v2/tenants/:tenant_id/databases/:name", get(get_database).delete(delete_database))
            .route(
                "/api/v2/tenants/:tenant_id/databases/:database_name/collections",
               post(create_collection).get(list_collections),
            )
            .route(
                "/api/v2/tenants/:tenant_id/databases/:database_name/collections_count",
                get(count_collections),
            )
            .route(
                "/api/v2/tenants/:tenant_id/databases/:database_name/collections/:collection_id",
                get(get_collection).put(update_collection).delete(delete_collection),
            )
            .route(
                "/api/v2/tenants/:tenant/databases/:database_name/collections/:collection_id/add",
                post(collection_add),
            )
            .route(
                "/api/v2/tenants/:tenant/databases/:database_name/collections/:collection_id/update",
                post(collection_update),
            )
            .route(
                "/api/v2/tenants/:tenant/databases/:database_name/collections/:collection_id/upsert",
                post(collection_upsert),
            )
            .route(
                "/api/v2/tenants/:tenant/databases/:database_name/collections/:collection_id/delete",
                post(collection_delete),
            )
            .route(
                "/api/v2/tenants/:tenant_id/databases/:database_name/collections/:collection_id/count",
                get(collection_count),
            )
            .route(
                "/api/v2/tenants/:tenant_id/databases/:database_name/collections/:collection_id/get",
                post(collection_get),
            )
            .route(
                "/api/v2/tenants/:tenant_id/databases/:database_name/collections/:collection_id/query",
                post(collection_query),
            )
            .with_state(server);
        let app = add_tracing_middleware(app);

        // TODO: configuration for this
        // TODO: tracing
        let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
        if circuit_breaker_config.enabled() {
            let service = AdmissionControlledService::new(circuit_breaker_config, app);
            axum::serve(listener, service.into_make_service())
                .await
                .unwrap();
        } else {
            axum::serve(listener, app).await.unwrap();
        };
    }
}

////////////////////////// Method Handlers //////////////////////////
// These handlers simply proxy the call and the relevant inputs into
// the appropriate method on the `FrontendServer` struct.

// Dummy implementation for now
async fn root() -> &'static str {
    "Chroma Rust Frontend"
}

async fn heartbeat(
    State(server): State<FrontendServer>,
) -> Result<Json<HeartbeatResponse>, ServerError> {
    Ok(Json(server.frontend.heartbeat().await?))
}

// Dummy implementation for now
async fn pre_flight_checks() -> Result<Json<ChecklistResponse>, ServerError> {
    Ok(Json(ChecklistResponse {
        max_batch_size: 100,
    }))
}

async fn reset(State(mut server): State<FrontendServer>) -> Result<Json<bool>, ServerError> {
    server.frontend.reset().await?;
    Ok(Json(true))
}

async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// Dummy implementation for now
async fn get_user_identity() -> Json<GetUserIdentityResponse> {
    Json(GetUserIdentityResponse {
        user_id: String::new(),
        tenant: "default_tenant".to_string(),
        databases: vec!["default_database".to_string()],
    })
}

async fn create_tenant(
    State(mut server): State<FrontendServer>,
    Json(request): Json<CreateTenantRequest>,
) -> Result<Json<CreateTenantResponse>, ServerError> {
    tracing::info!("Creating tenant [{}]", request.name);
    Ok(Json(server.frontend.create_tenant(request).await?))
}

async fn get_tenant(
    Path(name): Path<String>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<GetTenantResponse>, ServerError> {
    tracing::info!("Getting tenant [{}]", name);
    Ok(Json(
        server
            .frontend
            .get_tenant(GetTenantRequest { name })
            .await?,
    ))
}

#[derive(Deserialize, Debug)]
struct CreateDatabasePayload {
    name: String,
}

async fn create_database(
    Path(tenant_id): Path<String>,
    State(mut server): State<FrontendServer>,
    Json(CreateDatabasePayload { name }): Json<CreateDatabasePayload>,
) -> Result<Json<CreateDatabaseResponse>, ServerError> {
    tracing::info!("Creating database [{}] for tenant [{}]", name, tenant_id);
    let create_database_request = CreateDatabaseRequest {
        database_id: Uuid::new_v4(),
        tenant_id,
        database_name: name,
    };
    let res = server
        .frontend
        .create_database(create_database_request)
        .await?;
    Ok(Json(res))
}

#[derive(Deserialize)]
struct ListDatabasesPayload {
    limit: Option<u32>,
    offset: u32,
}

async fn list_databases(
    Path(tenant_id): Path<String>,
    State(mut server): State<FrontendServer>,
    Json(ListDatabasesPayload { limit, offset }): Json<ListDatabasesPayload>,
) -> Result<Json<ListDatabasesResponse>, ServerError> {
    tracing::info!("Listing database for tenant [{}]", tenant_id);
    Ok(Json(
        server
            .frontend
            .list_databases(ListDatabasesRequest {
                tenant_id,
                limit,
                offset,
            })
            .await?,
    ))
}

async fn get_database(
    Path((tenant_id, database_name)): Path<(String, String)>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<GetDatabaseResponse>, ServerError> {
    tracing::info!(
        "Getting database [{}] for tenant [{}]",
        database_name,
        tenant_id
    );
    let res = server
        .frontend
        .get_database(GetDatabaseRequest {
            tenant_id,
            database_name,
        })
        .await?;
    Ok(Json(res))
}

async fn delete_database(
    Path((tenant_id, database_name)): Path<(String, String)>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<DeleteDatabaseResponse>, ServerError> {
    tracing::info!(
        "Deleting database [{}] for tenant [{}]",
        database_name,
        tenant_id
    );
    Ok(Json(
        server
            .frontend
            .delete_database(DeleteDatabaseRequest {
                tenant_id,
                database_name,
            })
            .await?,
    ))
}

#[derive(Deserialize)]
struct ListCollectionsParams {
    limit: Option<u32>,
    #[serde(default)]
    offset: u32,
}

async fn list_collections(
    Path((tenant_id, database_name)): Path<(String, String)>,
    Query(ListCollectionsParams { limit, offset }): Query<ListCollectionsParams>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<ListCollectionsResponse>, ServerError> {
    tracing::info!(
        "Listing collections in database [{}] for tenant [{}] with limit [{:?}] and offset [{:?}]",
        database_name,
        tenant_id,
        limit,
        offset
    );
    Ok(Json(
        server
            .frontend
            .list_collections(ListCollectionsRequest {
                tenant_id,
                database_name,
                limit,
                offset,
            })
            .await?,
    ))
}

async fn count_collections(
    Path((tenant_id, database_name)): Path<(String, String)>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<CountCollectionsResponse>, ServerError> {
    tracing::info!(
        "Counting collections in database [{}] for tenant [{}]",
        database_name,
        tenant_id
    );
    Ok(Json(
        server
            .frontend
            .count_collections(CountCollectionsRequest {
                tenant_id,
                database_name,
            })
            .await?,
    ))
}

#[derive(Deserialize, Debug, Clone)]
pub struct CreateCollectionPayload {
    pub name: String,
    pub configuration: Option<serde_json::Value>,
    pub metadata: Option<Metadata>,
    pub get_or_create: bool,
}

async fn create_collection(
    Path((tenant_id, database_name)): Path<(String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<CreateCollectionPayload>,
) -> Result<Json<Collection>, ServerError> {
    tracing::info!("Creating collection in database [{database_name}] for tenant [{tenant_id}]");
    validate_name(&payload.name)?;
    if let Some(metadata) = payload.metadata.as_ref() {
        validate_non_empty_metadata(metadata)?;
    }
    let collection = server
        .frontend
        .create_collection(CreateCollectionRequest {
            name: payload.name,
            tenant_id,
            database_name,
            metadata: payload.metadata,
            configuration_json: payload.configuration,
            get_or_create: payload.get_or_create,
        })
        .await?;

    Ok(Json(collection))
}

async fn get_collection(
    Path((tenant_id, database_name, collection_name)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<Collection>, ServerError> {
    tracing::info!("Getting collection [{collection_name}] in database [{database_name}] for tenant [{tenant_id}]");
    let collection = server
        .frontend
        .get_collection(GetCollectionRequest {
            tenant_id,
            database_name,
            collection_name,
        })
        .await?;
    Ok(Json(collection))
}

#[derive(Deserialize, Debug, Clone)]
pub struct UpdateCollectionPayload {
    pub new_name: Option<String>,
    pub new_metadata: Option<UpdateMetadata>,
}

async fn update_collection(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<UpdateCollectionPayload>,
) -> Result<Json<UpdateCollectionResponse>, ServerError> {
    tracing::info!("Updating collection [{collection_id}] in database [{database_name}] for tenant [{tenant_id}]");
    if let Some(name) = payload.new_name.as_ref() {
        validate_name(name)?;
    }
    let collection_id =
        CollectionUuid::from_str(&collection_id).map_err(|_| ValidationError::CollectionId)?;

    if let Some(metadata) = payload.new_metadata.as_ref() {
        validate_non_empty_metadata(metadata)?;
    }

    server
        .frontend
        .update_collection(chroma_types::UpdateCollectionRequest {
            collection_id,
            new_name: payload.new_name,
            new_metadata: payload
                .new_metadata
                .map(CollectionMetadataUpdate::UpdateMetadata),
        })
        .await?;

    Ok(Json(UpdateCollectionResponse {}))
}

async fn delete_collection(
    Path((tenant_id, database_name, collection_name)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<UpdateCollectionResponse>, ServerError> {
    server
        .frontend
        .delete_collection(chroma_types::DeleteCollectionRequest {
            tenant_id,
            database_name,
            collection_name,
        })
        .await?;

    Ok(Json(UpdateCollectionResponse {}))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AddCollectionRecordsPayload {
    ids: Vec<String>,
    embeddings: Option<Vec<Vec<f32>>>,
    documents: Option<Vec<String>>,
    uris: Option<Vec<String>>,
    metadatas: Option<Vec<Metadata>>,
}

async fn collection_add(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<AddCollectionRecordsPayload>,
) -> Result<Json<AddCollectionRecordsResponse>, ServerError> {
    let collection_id =
        CollectionUuid(Uuid::parse_str(&collection_id).map_err(|_| ValidationError::CollectionId)?);

    server
        .frontend
        .validate_embedding(collection_id, payload.embeddings.as_ref(), true)
        .await?;

    let res = server
        .frontend
        .add(chroma_types::AddCollectionRecordsRequest {
            tenant_id,
            database_name,
            collection_id,
            ids: payload.ids,
            embeddings: payload.embeddings,
            documents: payload.documents,
            uris: payload.uris,
            metadatas: payload.metadatas,
        })
        .await?;

    Ok(Json(res))
}

#[derive(Deserialize, Debug, Clone)]
pub struct UpdateCollectionRecordsPayload {
    ids: Vec<String>,
    embeddings: Option<Vec<Vec<f32>>>,
    documents: Option<Vec<String>>,
    uris: Option<Vec<String>>,
    metadatas: Option<Vec<UpdateMetadata>>,
}

async fn collection_update(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<UpdateCollectionRecordsPayload>,
) -> Result<Json<UpdateCollectionRecordsResponse>, ServerError> {
    let collection_id =
        CollectionUuid(Uuid::parse_str(&collection_id).map_err(|_| ValidationError::CollectionId)?);

    server
        .frontend
        .validate_embedding(collection_id, payload.embeddings.as_ref(), true)
        .await?;

    Ok(Json(
        server
            .frontend
            .update(chroma_types::UpdateCollectionRecordsRequest {
                tenant_id,
                database_name,
                collection_id,
                ids: payload.ids,
                embeddings: payload.embeddings,
                documents: payload.documents,
                uris: payload.uris,
                metadatas: payload.metadatas,
            })
            .await?,
    ))
}

#[derive(Deserialize, Debug, Clone)]
pub struct UpsertCollectionRecordsPayload {
    ids: Vec<String>,
    embeddings: Option<Vec<Vec<f32>>>,
    documents: Option<Vec<String>>,
    uris: Option<Vec<String>>,
    metadatas: Option<Vec<UpdateMetadata>>,
}

async fn collection_upsert(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<UpsertCollectionRecordsPayload>,
) -> Result<Json<UpsertCollectionRecordsResponse>, ServerError> {
    let collection_id =
        CollectionUuid(Uuid::parse_str(&collection_id).map_err(|_| ValidationError::CollectionId)?);

    server
        .frontend
        .validate_embedding(collection_id, payload.embeddings.as_ref(), true)
        .await?;

    Ok(Json(
        server
            .frontend
            .upsert(chroma_types::UpsertCollectionRecordsRequest {
                tenant_id,
                database_name,
                collection_id,
                ids: payload.ids,
                embeddings: payload.embeddings,
                documents: payload.documents,
                uris: payload.uris,
                metadatas: payload.metadatas,
            })
            .await?,
    ))
}

#[derive(Deserialize, Debug, Clone)]
pub struct DeleteCollectionRecordsPayload {
    ids: Option<Vec<String>>,
    #[serde(flatten)]
    where_fields: RawWhereFields,
}

async fn collection_delete(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<DeleteCollectionRecordsPayload>,
) -> Result<Json<DeleteCollectionRecordsResponse>, ServerError> {
    let collection_id =
        CollectionUuid::from_str(&collection_id).map_err(|_| ValidationError::CollectionId)?;

    let r#where = payload.where_fields.parse()?;

    validate_non_empty_filter(&Filter {
        query_ids: payload.ids.clone(),
        where_clause: r#where.clone(),
    })?;

    server
        .frontend
        .delete(chroma_types::DeleteCollectionRecordsRequest {
            tenant_id,
            database_name,
            collection_id,
            ids: payload.ids,
            r#where,
        })
        .await?;

    Ok(Json(DeleteCollectionRecordsResponse {}))
}

async fn collection_count(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
) -> Result<Json<CountResponse>, ServerError> {
    tracing::info!(
        "Counting number of records in collection [{collection_id}] in database [{database_name}] for tenant [{tenant_id}]",
    );

    Ok(Json(
        server
            .frontend
            .count(CountRequest {
                tenant_id,
                database_name,
                collection_id: CollectionUuid::from_str(&collection_id)
                    .map_err(|_| ValidationError::CollectionId)?,
            })
            .await?,
    ))
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetRequestPayload {
    ids: Option<Vec<String>>,
    #[serde(flatten)]
    where_fields: RawWhereFields,
    limit: Option<u32>,
    offset: Option<u32>,
    #[serde(default = "IncludeList::default_get")]
    include: IncludeList,
}

async fn collection_get(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<GetRequestPayload>,
) -> Result<Json<GetResponse>, ServerError> {
    let collection_id =
        CollectionUuid::from_str(&collection_id).map_err(|_| ValidationError::CollectionId)?;
    tracing::info!(
        "Getting records from collection [{collection_id}] in database [{database_name}] for tenant [{tenant_id}]",
    );
    let res = server
        .frontend
        .get(GetRequest {
            tenant_id,
            database_name,
            collection_id,
            ids: payload.ids,
            r#where: payload.where_fields.parse()?,
            limit: payload.limit,
            offset: payload.offset.unwrap_or(0),
            include: payload.include,
        })
        .await?;
    Ok(Json(res))
}

#[derive(Deserialize, Debug, Clone)]
pub struct QueryRequestPayload {
    ids: Option<Vec<String>>,
    #[serde(flatten)]
    where_fields: RawWhereFields,
    query_embeddings: Vec<Vec<f32>>,
    n_results: Option<u32>,
    #[serde(default = "IncludeList::default_query")]
    include: IncludeList,
}

async fn collection_query(
    Path((tenant_id, database_name, collection_id)): Path<(String, String, String)>,
    State(mut server): State<FrontendServer>,
    Json(payload): Json<QueryRequestPayload>,
) -> Result<Json<QueryResponse>, ServerError> {
    let collection_id =
        CollectionUuid::from_str(&collection_id).map_err(|_| ValidationError::CollectionId)?;
    tracing::info!(
        "Querying records from collection [{collection_id}] in database [{database_name}] for tenant [{tenant_id}]",
    );

    server
        .frontend
        .validate_embedding(collection_id, Some(&payload.query_embeddings), true)
        .await?;

    let res = server
        .frontend
        .query(QueryRequest {
            tenant_id,
            database_name,
            collection_id,
            ids: payload.ids,
            r#where: payload.where_fields.parse()?,
            include: payload.include,
            embeddings: payload.query_embeddings,
            n_results: payload.n_results.unwrap_or(10),
        })
        .await?;

    Ok(Json(res))
}
