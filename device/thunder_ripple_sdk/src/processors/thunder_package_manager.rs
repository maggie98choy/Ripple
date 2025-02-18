// Copyright 2023 Comcast Cable Communications Management, LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use crate::ripple_sdk::{self};
use crate::{
    client::thunder_plugin::ThunderPlugin,
    ripple_sdk::{
        api::device::device_operator::{DeviceCallRequest, DeviceChannelParams, DeviceOperator},
        async_trait::async_trait,
        extn::{
            client::extn_client::ExtnClient,
            client::extn_processor::{
                DefaultExtnStreamer, ExtnRequestProcessor, ExtnStreamProcessor, ExtnStreamer,
            },
            extn_client_message::{ExtnMessage, ExtnResponse},
        },
        serde_json,
        tokio::sync::mpsc,
    },
    thunder_state::ThunderState,
};
use ripple_sdk::api::device::device_apps::AppMetadata;
use ripple_sdk::api::device::device_operator::{DeviceResponseMessage, DeviceSubscribeRequest};
use ripple_sdk::api::firebolt::fb_capabilities::FireboltPermissions;
use ripple_sdk::log::{debug, error, info};
use ripple_sdk::tokio;
use ripple_sdk::{
    api::device::device_apps::{AppsRequest, InstalledApp},
    framework::ripple_contract::RippleContract,
    utils::error::RippleError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// TODO: If/when ripple supports selectable download speeds we'll probably want multiple configurable values or compute this based on throughput.
const OPERATION_TIMEOUT_SECS: u64 = 6 * 60; // 6 minutes

#[derive(Debug, Clone)]
pub struct ThunderPackageManagerState {
    thunder_state: ThunderState,
    active_operations: Arc<Mutex<HashMap<String, Operation>>>,
}

#[derive(Debug)]
pub struct ThunderPackageManagerRequestProcessor {
    state: ThunderPackageManagerState,
    streamer: DefaultExtnStreamer,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetListRequest {
    pub id: String,
}

impl GetListRequest {
    pub fn new(id: String) -> GetListRequest {
        GetListRequest { id }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UninstallAppRequest {
    pub id: String,
    pub version: String,
    #[serde(rename = "type")]
    pub _type: String,
    pub uninstall_type: String,
}

impl UninstallAppRequest {
    pub fn new(app: InstalledApp) -> UninstallAppRequest {
        UninstallAppRequest {
            id: app.id,
            version: app.version,
            _type: String::default(),
            uninstall_type: String::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstallAppRequest {
    pub id: String,
    pub version: String,
    pub url: String,
    pub app_name: String,
    #[serde(rename = "type")]
    pub _type: String,
    pub category: String,
}

impl InstallAppRequest {
    pub fn new(app: AppMetadata) -> InstallAppRequest {
        let mut app_type = String::default();
        let mut category = String::default();

        if let Some(data_json) = app.data {
            if let Ok(data) = serde_json::from_str::<HashMap<String, String>>(&data_json) {
                if let Some(t) = data.get("type") {
                    app_type = t.to_string();
                }
                if let Some(c) = data.get("category") {
                    category = c.to_string();
                }
            }
        }

        InstallAppRequest {
            id: app.id,
            version: app.version,
            url: app.uri,
            app_name: app.title,
            _type: app_type,
            category,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelRequest {
    pub handle: String,
}

impl CancelRequest {
    pub fn new(handle: String) -> CancelRequest {
        CancelRequest { handle }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetMetadataRequest {
    pub id: String,
    pub version: String,
}

impl GetMetadataRequest {
    pub fn new(id: String, version: String) -> GetMetadataRequest {
        GetMetadataRequest { id, version }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AppData {
    pub version: String,
}

impl AppData {
    pub fn new(version: String) -> AppData {
        AppData { version }
    }
}

#[derive(Debug, PartialEq)]
struct Operation {
    durable_app_id: String,
    operation_type: AppsOperationType,
    app_data: AppData,
}

impl Operation {
    pub fn new(
        operation_type: AppsOperationType,
        durable_app_id: String,
        app_data: AppData,
    ) -> Operation {
        Operation {
            operation_type,
            durable_app_id,
            app_data,
        }
    }
}

#[derive(Debug)]
enum OperationStatus {
    Succeeded,
    Failed,
    NotStarted,
    Downloading,
    Downloaded,
    DownloadFailed,
    Verifying,
    VerificationFailed,
    InstallationFailed,
    Cancelled,
    NotEnoughStorage,
    Unknown,
}

impl OperationStatus {
    pub fn new(s: &str) -> OperationStatus {
        match s {
            "Succeeded" => OperationStatus::Succeeded,
            "Failed" => OperationStatus::Failed,
            "NotStarted" => OperationStatus::NotStarted,
            "Downloading" => OperationStatus::Downloading,
            "Downloaded" => OperationStatus::Downloaded,
            "DownloadFailed" => OperationStatus::DownloadFailed,
            "Verifying" => OperationStatus::Verifying,
            "VerificationFailed" => OperationStatus::VerificationFailed,
            "InstallationFailed" => OperationStatus::InstallationFailed,
            "Cancelled" => OperationStatus::Cancelled,
            "NotEnoughStorage" => OperationStatus::NotEnoughStorage,
            "Unknown" => OperationStatus::Unknown,
            _ => OperationStatus::Unknown,
        }
    }

    pub fn completed(&self) -> bool {
        match self {
            OperationStatus::Succeeded => true,
            OperationStatus::Failed => true,
            OperationStatus::NotStarted => false,
            OperationStatus::Downloading => false,
            OperationStatus::Downloaded => false,
            OperationStatus::DownloadFailed => true,
            OperationStatus::Verifying => false,
            OperationStatus::VerificationFailed => true,
            OperationStatus::InstallationFailed => true,
            OperationStatus::Cancelled => true,
            OperationStatus::NotEnoughStorage => true,
            OperationStatus::Unknown => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppsOperationType {
    Install,
    Uninstall,
}

impl FromStr for AppsOperationType {
    type Err = ();

    fn from_str(input: &str) -> Result<AppsOperationType, Self::Err> {
        match input.to_lowercase().as_str() {
            "install" => Ok(AppsOperationType::Install),
            "uninstall" => Ok(AppsOperationType::Uninstall),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppsOperationStatus {
    pub handle: String,
    pub operation: AppsOperationType,
    #[serde(rename = "type")]
    pub app_type: String,
    pub id: String,
    pub version: String,
    pub status: String,
    pub details: String,
}

impl AppsOperationStatus {
    pub fn new(
        handle: String,
        operation: AppsOperationType,
        app_type: String,
        id: String,
        version: String,
        status: String,
        details: String,
    ) -> AppsOperationStatus {
        AppsOperationStatus {
            handle,
            operation,
            app_type,
            id,
            version,
            status,
            details,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub appname: String,
    #[serde(rename = "type")]
    pub _type: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KeyValuePair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThunderAppMetadata {
    pub metadata: Metadata,
    pub resources: Vec<KeyValuePair>,
}

fn get_string_field(
    status: &serde_json::Map<std::string::String, serde_json::Value>,
    field_name: &str,
) -> String {
    if let Some(Value::String(field_value)) = status.get(field_name) {
        return field_value.clone();
    }
    String::default()
}

impl ThunderPackageManagerRequestProcessor {
    pub fn new(thunder_state: ThunderState) -> ThunderPackageManagerRequestProcessor {
        ThunderPackageManagerRequestProcessor {
            state: ThunderPackageManagerState {
                thunder_state,
                active_operations: Arc::new(Mutex::new(HashMap::default())),
            },
            streamer: DefaultExtnStreamer::new(),
        }
    }

    pub async fn init(&self, thunder_state: ThunderState) {
        let (sub_tx, mut sub_rx) = mpsc::channel::<DeviceResponseMessage>(32);

        debug!("ThunderPackageManagerRequestProcessor::init: Starting listener loop");

        let state = self.state.clone();
        tokio::spawn(async move {
            while let Some(message) = sub_rx.recv().await {
                debug!(
                    "ThunderPackageManagerRequestProcessor::: message={:?}",
                    message
                );
                if let Some(status_map) = message.message.as_object() {
                    let operation_type = AppsOperationType::from_str(
                        get_string_field(status_map, "operation").as_str(),
                    );

                    if let Err(()) = operation_type {
                        error!("ThunderPackageManagerRequestProcessor: Unexpected operation type");
                        continue;
                    }

                    let operation_status = AppsOperationStatus {
                        handle: get_string_field(status_map, "handle"),
                        operation: operation_type.unwrap(),
                        app_type: get_string_field(status_map, "type"),
                        id: get_string_field(status_map, "id"),
                        version: get_string_field(status_map, "version"),
                        status: get_string_field(status_map, "status"),
                        details: get_string_field(status_map, "details"),
                    };

                    if OperationStatus::new(&operation_status.status).completed() {
                        let operation = Operation::new(
                            operation_status.operation.clone(),
                            operation_status.id.clone(),
                            AppData::new(operation_status.version.clone()),
                        );
                        Self::add_or_remove_operation(
                            state.clone(),
                            operation_status.handle,
                            operation,
                        );
                    }
                } else {
                    error!("ThunderPackageManagerRequestProcessor: Unexpected message payload");
                }
            }
        });

        thunder_state
            .get_thunder_client()
            .clone()
            .subscribe(
                DeviceSubscribeRequest {
                    module: ThunderPlugin::PackageManager.callsign_and_version(),
                    event_name: "operationstatus".into(),
                    params: None,
                    sub_id: None,
                },
                sub_tx,
            )
            .await;
    }

    // add_or_remove_operation: Adds or removes an active operation to/from the map depending on whether or not it already existed.
    // This is necessary because it's possible for thunder to send an operation status event before the associated thunder call
    // returns. This allows us to track operations by handle regardless of which occurs first, e.g. if the thunder call returns before
    // the event is received, the operation is added to the map upon return and removed when the event arrives. If the event occurs before
    // the thunder call returns, the operation is added when the event occurs and removed when the call returns. The active operation map
    // is used to cancel any operations that haven't completed after some time.
    fn add_or_remove_operation(
        state: ThunderPackageManagerState,
        handle: String,
        operation: Operation,
    ) {
        if state
            .active_operations
            .lock()
            .unwrap()
            .remove(&handle)
            .is_none()
        {
            state
                .active_operations
                .lock()
                .unwrap()
                .insert(handle.clone(), operation);
            Self::start_operation_timer(state, handle, None);
        }
    }

    fn operation_in_progress(
        state: ThunderPackageManagerState,
        operation_type: AppsOperationType,
        app_id: &str,
        version: &str,
    ) -> Option<String> {
        debug!(
            "operation_in_progress: operation_type={:?}, app_id={}, version={}",
            operation_type, app_id, version
        );
        for (handle, operation) in state.active_operations.lock().unwrap().iter() {
            if operation_type == operation.operation_type
                && app_id.eq(&operation.durable_app_id)
                && version.eq(&operation.app_data.version)
            {
                return Some(handle.to_string());
            }
        }
        None
    }

    fn start_operation_timer(
        state: ThunderPackageManagerState,
        handle: String,
        timeout_secs: Option<u64>,
    ) {
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(
                timeout_secs.unwrap_or(OPERATION_TIMEOUT_SECS),
            ))
            .await;
            if state
                .active_operations
                .lock()
                .unwrap()
                .remove(&handle)
                .is_some()
            {
                error!(
                    "Detected incomplete operation, attempting to cancel: handle={}",
                    handle.clone()
                );

                Self::cancel_operation(state.thunder_state, handle).await;
            }
        });
    }

    async fn get_apps_list(thunder_state: ThunderState, id: Option<String>) -> ExtnResponse {
        let method: String = ThunderPlugin::PackageManager.method("getlist");
        let request = GetListRequest::new(id.unwrap_or_default());
        let device_response = thunder_state
            .get_thunder_client()
            .call(DeviceCallRequest {
                method,
                params: Some(DeviceChannelParams::Json(
                    serde_json::to_string(&request).unwrap(),
                )),
            })
            .await;
        match serde_json::from_value::<Vec<InstalledApp>>(device_response.message) {
            Ok(apps) => ExtnResponse::InstalledApps(apps),
            Err(_) => ExtnResponse::Error(RippleError::ProcessorError),
        }
    }

    async fn get_apps(thunder_state: ThunderState, req: ExtnMessage, id: Option<String>) -> bool {
        let res = Self::get_apps_list(thunder_state.clone(), id).await;
        Self::respond(thunder_state.get_client(), req, res)
            .await
            .is_ok()
    }

    async fn install_app(
        state: ThunderPackageManagerState,
        req: ExtnMessage,
        app: AppMetadata,
    ) -> bool {
        if let Some(handle) = Self::operation_in_progress(
            state.clone(),
            AppsOperationType::Install,
            &app.id,
            &app.version,
        ) {
            info!(
                "install_app: Installation already in progress: app={}, version={}",
                app.id, app.version
            );

            return Self::respond(
                state.clone().thunder_state.get_client(),
                req,
                ExtnResponse::String(handle),
            )
            .await
            .is_ok();
        }

        let method: String = ThunderPlugin::PackageManager.method("install");
        let request = InstallAppRequest::new(app.clone());
        let device_response = state
            .thunder_state
            .get_thunder_client()
            .call(DeviceCallRequest {
                method,
                params: Some(DeviceChannelParams::Json(
                    serde_json::to_string(&request).unwrap(),
                )),
            })
            .await;
        let res = match serde_json::from_value::<String>(device_response.message) {
            Ok(handle) => {
                let operation = Operation::new(
                    AppsOperationType::Install,
                    app.id,
                    AppData::new(app.version),
                );
                Self::add_or_remove_operation(state.clone(), handle.clone(), operation);
                ExtnResponse::String(handle)
            }
            Err(_) => ExtnResponse::Error(RippleError::ProcessorError),
        };

        Self::respond(state.thunder_state.get_client(), req, res)
            .await
            .is_ok()
    }

    async fn uninstall_app(
        state: ThunderPackageManagerState,
        req: ExtnMessage,
        app: InstalledApp,
    ) -> bool {
        if let Some(handle) = Self::operation_in_progress(
            state.clone(),
            AppsOperationType::Uninstall,
            &app.id,
            &app.version,
        ) {
            info!(
                "uninstall_app: Uninstallation already in progress: app={}, version={}",
                app.id, app.version
            );

            return Self::respond(
                state.clone().thunder_state.get_client(),
                req,
                ExtnResponse::String(handle),
            )
            .await
            .is_ok();
        }

        let method: String = ThunderPlugin::PackageManager.method("uninstall");
        let request = UninstallAppRequest::new(app.clone());
        let device_response = state
            .thunder_state
            .get_thunder_client()
            .call(DeviceCallRequest {
                method,
                params: Some(DeviceChannelParams::Json(
                    serde_json::to_string(&request).unwrap(),
                )),
            })
            .await;
        let res = match serde_json::from_value::<String>(device_response.message) {
            Ok(handle) => {
                let operation = Operation::new(
                    AppsOperationType::Uninstall,
                    app.id,
                    AppData::new(app.version),
                );
                Self::add_or_remove_operation(state.clone(), handle.clone(), operation);
                ExtnResponse::String(handle)
            }
            Err(_) => ExtnResponse::Error(RippleError::ProcessorError),
        };

        Self::respond(state.thunder_state.get_client(), req, res)
            .await
            .is_ok()
    }

    fn decode_permissions(perms_encoded: String) -> Result<FireboltPermissions, ()> {
        let perms = base64::decode(perms_encoded);
        if let Err(e) = perms {
            error!(
                "decode_permissions: Could not decode permissions: e={:?}",
                e
            );
            return Err(());
        }

        let perms = perms.unwrap();
        let perms_str = String::from_utf8_lossy(&perms);

        debug!("decode_permissions: perms={}", perms_str.clone());

        let firebolt_perms = serde_json::from_str::<FireboltPermissions>(&perms_str);

        if let Err(e) = firebolt_perms {
            error!(
                "decode_permissions: Could not deserialize permissions: e={:?}",
                e
            );
            return Err(());
        }

        Ok(firebolt_perms.unwrap())
    }

    async fn get_firebolt_permissions(
        state: ThunderPackageManagerState,
        req: ExtnMessage,
        app_id: String,
    ) -> bool {
        let installed_apps =
            match Self::get_apps_list(state.thunder_state.clone(), Some(app_id.clone())).await {
                ExtnResponse::InstalledApps(apps) => apps,
                _ => {
                    error!("get_firebolt_permissions: Unexpected extension response");
                    return Self::respond(
                        state.thunder_state.get_client(),
                        req,
                        ExtnResponse::Error(RippleError::ProcessorError),
                    )
                    .await
                    .is_ok();
                }
            };

        let installed_app = installed_apps.iter().find(|app| app.id.eq(&app_id));
        if installed_app.is_none() {
            error!("get_firebolt_permissions: Failed to determine version");
            return Self::respond(
                state.thunder_state.get_client(),
                req,
                ExtnResponse::Error(RippleError::ProcessorError),
            )
            .await
            .is_ok();
        }

        let app = installed_app.unwrap();
        let method: String = ThunderPlugin::PackageManager.method("getmetadata");
        let request = GetMetadataRequest::new(app.id.clone(), app.version.clone());
        let device_response = state
            .thunder_state
            .get_thunder_client()
            .call(DeviceCallRequest {
                method,
                params: Some(DeviceChannelParams::Json(
                    serde_json::to_string(&request).unwrap(),
                )),
            })
            .await;

        debug!(
            "get_firebolt_permissions: device_response={:?}",
            device_response
        );

        let res = match serde_json::from_value::<ThunderAppMetadata>(device_response.message) {
            Ok(metadata) => {
                match metadata
                    .resources
                    .iter()
                    .find(|resource| resource.key.eq(&String::from("firebolt")))
                {
                    Some(r) => match Self::decode_permissions(r.value.clone()) {
                        Ok(permissions) => ExtnResponse::Permission(permissions.capabilities),
                        Err(()) => ExtnResponse::Error(RippleError::ProcessorError),
                    },
                    None => {
                        error!("get_firebolt_permissions: No permissions for app");
                        ExtnResponse::Error(RippleError::ProcessorError)
                    }
                }
            }
            Err(e) => {
                error!(
                    "get_firebolt_permissions: Failed to deserialize response: e={:?}",
                    e
                );
                ExtnResponse::Error(RippleError::ProcessorError)
            }
        };

        Self::respond(state.thunder_state.get_client(), req, res)
            .await
            .is_ok()
    }

    async fn cancel_operation(thunder_state: ThunderState, handle: String) {
        let method: String = ThunderPlugin::PackageManager.method("cancel");
        let request = CancelRequest::new(handle);
        let device_response = thunder_state
            .get_thunder_client()
            .call(DeviceCallRequest {
                method,
                params: Some(DeviceChannelParams::Json(
                    serde_json::to_string(&request).unwrap(),
                )),
            })
            .await;

        if !device_response.message.is_null() {
            error!(
                "cancel_operation: Unexpected response: message={:?}",
                device_response.message
            );
        }
    }
}

impl ExtnStreamProcessor for ThunderPackageManagerRequestProcessor {
    type STATE = ThunderPackageManagerState;
    type VALUE = AppsRequest;

    fn get_state(&self) -> Self::STATE {
        self.state.clone()
    }

    fn receiver(&mut self) -> mpsc::Receiver<ExtnMessage> {
        self.streamer.receiver()
    }

    fn sender(&self) -> mpsc::Sender<ExtnMessage> {
        self.streamer.sender()
    }

    fn contract(&self) -> RippleContract {
        RippleContract::Apps
    }
}

#[async_trait]
impl ExtnRequestProcessor for ThunderPackageManagerRequestProcessor {
    fn get_client(&self) -> ExtnClient {
        self.state.thunder_state.get_client()
    }
    async fn process_request(
        state: Self::STATE,
        msg: ExtnMessage,
        extracted_message: Self::VALUE,
    ) -> bool {
        match extracted_message {
            AppsRequest::GetApps(id) => Self::get_apps(state.thunder_state.clone(), msg, id).await,
            AppsRequest::InstallApp(app) => Self::install_app(state.clone(), msg, app).await,
            AppsRequest::UninstallApp(app) => Self::uninstall_app(state.clone(), msg, app).await,
            AppsRequest::GetFireboltPermissions(app_id) => {
                Self::get_firebolt_permissions(state.clone(), msg, app_id).await
            }
        }
    }
}
