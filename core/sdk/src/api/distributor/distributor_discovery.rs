// If not stated otherwise in this file or this component's license file the
// following copyright and licenses apply:
//
// Copyright 2023 RDK Management
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

use serde::{Deserialize, Serialize};

use crate::{
    api::firebolt::fb_discovery::{
        ClearContentSetParams, ContentAccessListSetParams, MediaEventsAccountLinkRequestParams,
        SignInRequestParams,
    },
    extn::extn_client_message::{ExtnPayload, ExtnPayloadProvider, ExtnRequest},
    framework::ripple_contract::RippleContract,
};

use super::distributor_request::DistributorRequest;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum DiscoveryRequest {
    SetContentAccess(ContentAccessListSetParams),
    ClearContent(ClearContentSetParams),
    SignIn(SignInRequestParams),
}

impl ExtnPayloadProvider for DiscoveryRequest {
    fn get_from_payload(payload: ExtnPayload) -> Option<Self> {
        match payload {
            ExtnPayload::Request(r) => match r {
                ExtnRequest::Distributor(v) => match v {
                    DistributorRequest::Discovery(d) => {
                        return Some(d);
                    }
                    _ => {}
                },
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn get_extn_payload(&self) -> ExtnPayload {
        ExtnPayload::Request(ExtnRequest::Distributor(DistributorRequest::Discovery(
            self.clone(),
        )))
    }

    fn contract() -> RippleContract {
        RippleContract::Discovery
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum MediaEventRequest {
    MediaEventAccountLink(MediaEventsAccountLinkRequestParams),
}

impl ExtnPayloadProvider for MediaEventRequest {
    fn get_from_payload(payload: ExtnPayload) -> Option<Self> {
        match payload {
            ExtnPayload::Request(r) => match r {
                ExtnRequest::Distributor(v) => match v {
                    DistributorRequest::MediaEvent(d) => {
                        return Some(d);
                    }
                    _ => {}
                },
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn get_extn_payload(&self) -> ExtnPayload {
        ExtnPayload::Request(ExtnRequest::Distributor(DistributorRequest::MediaEvent(
            self.clone(),
        )))
    }

    fn contract() -> RippleContract {
        RippleContract::MediaEvents
    }
}
